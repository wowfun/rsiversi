use super::*;
use rsi_sandbox::{
    ConfinedProcess, EnforcementStamp, ProcessStdio, SandboxBackend, SandboxFileSystem,
    SandboxMode, SandboxNetwork, SandboxScratch,
};
fn fixture_spec(workspace: &std::path::Path, program: &str, args: &[&str]) -> DuplexProcessSpec {
    DuplexProcessSpec {
        process: ConfinedProcess {
            owner: None,
            stdio: ProcessStdio::Pipes,
            program: program.into(),
            arguments: args.iter().map(std::ffi::OsString::from).collect(),
            cwd: workspace.to_owned(),
            stamp: EnforcementStamp {
                requested: SandboxMode::DangerFullAccess,
                backend: SandboxBackend::Unconfined,
                workspace: workspace.to_owned(),
                filesystem: SandboxFileSystem::Unconfined,
                scratch: SandboxScratch::Host,
                network: SandboxNetwork::Host,
            },
        },
        environment: vec![("SystemRoot".into(), std::env::var_os("SystemRoot").unwrap())],
        stdout_buffer_bytes: 64 * 1024,
        stderr_max_bytes: 1024,
        termination_grace_ms: 1000,
    }
}

#[tokio::test]
async fn duplex_acknowledgement_allows_the_woken_next_writer() {
    use std::{
        future::{Future as _, poll_fn},
        pin::pin,
        sync::atomic::AtomicU8,
        task::{Context, Poll, Wake, Waker},
    };
    struct NextWrite {
        input: Arc<dyn DuplexInput>,
        observed: Arc<AtomicU8>,
        caller: Waker,
    }
    impl Wake for NextWrite {
        fn wake(self: Arc<Self>) {
            self.wake_by_ref();
        }
        fn wake_by_ref(self: &Arc<Self>) {
            if self
                .observed
                .compare_exchange(0, 4, Ordering::SeqCst, Ordering::SeqCst)
                .is_ok()
            {
                // Poll the next write during the acknowledgement's wake, before
                // the worker can proceed. This does not depend on thread timing.
                let mut next = pin!(self.input.write(b"\r\n"));
                let observed = match next.as_mut().poll(&mut Context::from_waker(Waker::noop())) {
                    Poll::Pending => 1,
                    Poll::Ready(Err(ProcessError::Capacity)) => 2,
                    Poll::Ready(_) => 3,
                };
                self.observed.store(observed, Ordering::SeqCst);
            }
            self.caller.wake_by_ref();
        }
    }
    let temporary = tempfile::tempdir().unwrap();
    let service = Service::new(ProcessLocalConfig::default());
    let process = service
        .spawn_duplex(fixture_spec(
            temporary.path(),
            r"C:\Windows\System32\cmd.exe",
            &["/d", "/v:on", "/c", "set /p line=&echo !line!"],
        ))
        .await
        .unwrap();
    let input = process.stdin();
    let observed = Arc::new(AtomicU8::new(0));
    let mut first = pin!(input.write(b"x"));
    let result = tokio::time::timeout(
        Duration::from_secs(10),
        poll_fn(|cx| {
            let wake = Waker::from(Arc::new(NextWrite {
                input: input.clone(),
                observed: observed.clone(),
                caller: cx.waker().clone(),
            }));
            first.as_mut().poll(&mut Context::from_waker(&wake))
        }),
    )
    .await;
    process.terminate();
    process.wait_settlement().await.unwrap();
    service.shutdown().await.unwrap();
    assert_eq!(result.unwrap().unwrap(), 1);
    assert_eq!(
        observed.load(Ordering::SeqCst),
        1,
        "the acknowledged writer still holds admission"
    );
}

#[tokio::test]
async fn duplex_slow_reader_resumes_without_losing_or_replaying_bytes() {
    let temporary = tempfile::tempdir().unwrap();
    let service = Service::new(ProcessLocalConfig::default());
    let process = service.spawn_duplex(fixture_spec(
        temporary.path(), r"C:\Windows\System32\WindowsPowerShell\v1.0\powershell.exe",
        &["-NoProfile", "-NonInteractive", "-Command",
            "$output = [Console]::OpenStandardOutput(); $output.WriteByte(114); $output.Flush(); [Threading.Thread]::Sleep(1200); [Console]::OpenStandardInput().CopyTo($output)"],
    )).await.unwrap();
    let input = process.stdin();
    let output = process.stdout();
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(10), output.read(1))
            .await
            .unwrap()
            .unwrap()
            .bytes,
        b"r"
    );
    let bytes: Vec<_> = (0..1024 * 1024)
        .map(|index| u8::try_from(index % 251).unwrap())
        .collect();
    let result = tokio::time::timeout(Duration::from_secs(15), async {
        tokio::try_join!(
            async {
                for chunk in bytes.chunks(MAXIMUM_DUPLEX_CHUNK_BYTES) {
                    let mut rest = chunk;
                    while !rest.is_empty() {
                        let accepted = input.write(rest).await?;
                        rest = &rest[accepted..];
                    }
                }
                input.close().await
            },
            async {
                let mut received = Vec::new();
                loop {
                    let read = output.read(MAXIMUM_DUPLEX_CHUNK_BYTES).await?;
                    received.extend(read.bytes);
                    if read.eof {
                        return Ok::<_, ProcessError>(received);
                    }
                }
            }
        )
    })
    .await;
    if !matches!(result, Ok(Ok(_))) {
        process.terminate();
    }
    process.wait_settlement().await.unwrap();
    service.shutdown().await.unwrap();
    let ((), received) = result
        .unwrap()
        .expect("healthy pipe backpressure must remain pending");
    assert_eq!(received, bytes);
    assert_eq!(process.wait().await.unwrap().exit_code, Some(0));
}

#[tokio::test]
async fn duplex_termination_reaps_a_child_with_a_blocked_native_write() {
    let temporary = tempfile::tempdir().unwrap();
    let service = Service::new(ProcessLocalConfig::default());
    let process = service
        .spawn_duplex(fixture_spec(
            temporary.path(),
            r"C:\Windows\System32\WindowsPowerShell\v1.0\powershell.exe",
            &["-NoProfile", "-NonInteractive", "-Command",
                "$output = [Console]::OpenStandardOutput(); $output.WriteByte(114); $output.Flush(); [Threading.Thread]::Sleep(30000)"],
        ))
        .await
        .unwrap();
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(10), process.stdout().read(1))
            .await
            .unwrap()
            .unwrap()
            .bytes,
        b"r"
    );
    let input = process.stdin();
    let bytes = vec![42; MAXIMUM_DUPLEX_CHUNK_BYTES];
    let mut write = std::pin::pin!(async {
        for _ in 0..16 {
            let mut rest = bytes.as_slice();
            while !rest.is_empty() {
                let accepted = input.write(rest).await?;
                rest = &rest[accepted..];
            }
        }
        Ok::<_, ProcessError>(())
    });
    let pending = tokio::time::timeout(Duration::from_millis(100), &mut write).await;
    process.terminate();
    tokio::time::timeout(Duration::from_secs(5), process.wait_settlement())
        .await
        .unwrap()
        .unwrap();
    service.shutdown().await.unwrap();
    assert!(
        pending.is_err(),
        "native write must still await the non-reading child"
    );
    assert!(matches!(write.await, Err(ProcessError::Io(_))));
}
#[tokio::test]
async fn native_duplex_round_trip_preserves_bytes_under_small_backpressure_window() {
    let temporary = tempfile::tempdir().unwrap();
    let service = Service::new(ProcessLocalConfig::default());
    let mut spec = fixture_spec(
        temporary.path(),
        r"C:\Windows\System32\cmd.exe",
        &["/d", "/v:on", "/c", "set /p line=&echo !line!"],
    );
    spec.stdout_buffer_bytes = 4;
    spec.stderr_max_bytes = 8;
    let process = service.spawn_duplex(spec).await.unwrap();
    assert_eq!(process.stdin().write(b"abcdef\r\n").await.unwrap(), 8);
    process.stdin().close().await.unwrap();
    let mut output = Vec::new();
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let read = process.stdout().read(2).await.unwrap();
            output.extend(read.bytes);
            if read.eof {
                break;
            }
        }
    })
    .await
    .unwrap();
    assert_eq!(output, b"abcdef\r\n");
    assert_eq!(process.wait().await.unwrap().exit_code, Some(0));
    process.wait_settlement().await.unwrap();
    service.shutdown().await.unwrap();
}
