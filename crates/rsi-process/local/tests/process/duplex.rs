use super::*;
use rsi_process::{DuplexProcess, DuplexProcessContract, DuplexProcessSpec, ManagedDuplexProcess};
use std::time::Duration;
fn duplex_spec(script: &str, capacity: usize) -> DuplexProcessSpec {
    DuplexProcessSpec {
        process: unconfined_shell(script),
        environment: vec![],
        stdout_buffer_bytes: capacity,
        stderr_max_bytes: 1024,
        termination_grace_ms: 1000,
    }
}
async fn owners(
    config: serde_json::Value,
) -> (
    rsi_meta::FiberHandle,
    Arc<dyn Process>,
    Arc<dyn DuplexProcess>,
) {
    let runtime = Runtime::default();
    let fiber = runtime
        .root()
        .apply(
            ResolvedFactory::linked(
                "rsi.process.local",
                "test",
                UpdateMode::Replayable,
                Arc::new(ProcessLocalFactory),
            ),
            config,
        )
        .await
        .unwrap();
    (
        fiber,
        runtime.root().lookup_local::<ProcessContract>().unwrap(),
        runtime
            .root()
            .lookup_local::<DuplexProcessContract>()
            .unwrap(),
    )
}
async fn write_all(process: &ManagedDuplexProcess, mut bytes: &[u8]) {
    let stdin = process.stdin();
    while !bytes.is_empty() {
        let count = stdin.write(&bytes[..bytes.len().min(65536)]).await.unwrap();
        assert!(count > 0);
        bytes = &bytes[count..];
    }
}
#[tokio::test]
async fn stdout_half_close_is_observable_before_child_exit_without_releasing_admission() {
    let (fiber, batch, duplex) = owners(json!({"maximum_active_processes":1})).await;
    let managed = duplex
        .spawn(duplex_spec("printf ready; exec 1>&-; read token || :", 128))
        .unwrap();
    let output = managed.stdout();
    let bytes = tokio::time::timeout(Duration::from_secs(2), async {
        let mut bytes = Vec::new();
        loop {
            let page = output.read(2).await.unwrap();
            bytes.extend(page.bytes);
            if page.eof {
                break bytes;
            }
        }
    })
    .await
    .expect("stdout EOF must not await the child waiting on stdin");
    assert_eq!(bytes, b"ready");
    assert!(
        tokio::time::timeout(Duration::from_millis(20), managed.wait())
            .await
            .is_err()
    );
    assert!(matches!(
        batch.spawn(spec("exit 0", 1)),
        Err(ProcessError::Capacity)
    ));
    managed.stdin().close().await.unwrap();
    assert_eq!(managed.wait().await.unwrap().exit_code, Some(0));
    assert!(output.read(1).await.unwrap().eof);
    let next = batch.spawn(spec("exit 0", 1)).unwrap();
    next.wait().await.unwrap();
    assert!(fiber.dispose().await.is_clean());
}

#[tokio::test]
async fn persistent_binary_echo_is_lossless_through_a_tiny_backpressured_queue() {
    let (fiber, _, process) = owners(json!({})).await;
    let managed = process.spawn(duplex_spec("exec /bin/cat", 257)).unwrap();
    let expected: Vec<_> = (0..768 * 1024)
        .map(|i| u8::try_from(i % 251).unwrap())
        .collect();
    let writer = managed.clone();
    let bytes = expected.clone();
    let writing = tokio::spawn(async move {
        write_all(&writer, &bytes).await;
        write_all(&writer, b"second message\0").await;
        writer.stdin().close().await.unwrap();
    });
    let mut actual = vec![];
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            let page = managed.stdout().read(113).await.unwrap();
            assert!(page.bytes.len() <= 113);
            assert!(!page.bytes.is_empty() || page.eof);
            actual.extend(page.bytes);
            if page.eof {
                break;
            }
        }
    })
    .await
    .unwrap();
    writing.await.unwrap();
    let mut expected = expected;
    expected.extend(b"second message\0");
    assert_eq!(actual, expected);
    assert_eq!(managed.wait().await.unwrap().exit_code, Some(0));
    assert!(managed.stdin().write(b"late").await.is_err());
    drop(managed);
    assert!(fiber.dispose().await.is_clean());
}
#[tokio::test]
async fn duplex_and_batch_share_slots_and_capture_retention_including_the_write_buffer() {
    let (fiber, batch, duplex) =
        owners(json!({"maximum_active_processes":1,"maximum_capture_bytes":67584})).await;
    let managed = duplex.spawn(duplex_spec("exec /bin/cat", 1024)).unwrap();
    let output = managed.stdout();
    assert!(matches!(
        batch.spawn(spec("exit 0", 1)),
        Err(ProcessError::Capacity)
    ));
    managed.stdin().close().await.unwrap();
    managed.wait().await.unwrap();
    // Reaping returns process admission; retained protocol ports still own all 64 KiB+2 KiB capture.
    assert!(matches!(
        batch.spawn(spec("exit 0", 1)),
        Err(ProcessError::Capacity)
    ));
    drop(managed);
    assert!(matches!(
        batch.spawn(spec("exit 0", 1)),
        Err(ProcessError::Capacity)
    ));
    drop(output);
    let next = batch.spawn(spec("exit 0", 1)).unwrap();
    next.wait().await.unwrap();
    drop(next);
    let batch_active = batch.spawn(spec("exec /bin/cat", 1)).unwrap();
    batch_active.wait().await.unwrap();
    drop(batch_active);
    assert!(fiber.dispose().await.is_clean());
}
#[tokio::test]
async fn termination_unblocks_unread_output_and_reaps_the_child() {
    let (fiber, _, duplex) = owners(json!({})).await;
    let managed = duplex
        .spawn(duplex_spec("while :; do printf 0123456789; done", 64))
        .unwrap();
    let first = managed.stdout().read(1).await.unwrap();
    assert_eq!(first.bytes, b"0");
    let pid = managed.pid();
    managed.terminate();
    let outcome = tokio::time::timeout(Duration::from_secs(3), managed.wait())
        .await
        .unwrap();
    assert!(
        matches!(outcome, Err(ProcessError::Io(_))),
        "intentional stdout cancellation remains incomplete: {outcome:?}"
    );
    managed.wait_settlement().await.unwrap();
    assert_eq!(
        rustix::process::test_kill_process(
            rustix::process::Pid::from_raw(i32::try_from(pid).unwrap()).unwrap()
        ),
        Err(rustix::io::Errno::SRCH)
    );
    let mut ended = false;
    for _ in 0..128 {
        match managed.stdout().read(64).await {
            Ok(page) if page.eof => {
                ended = true;
                break;
            }
            Err(_) => {
                ended = true;
                break;
            }
            _ => {}
        }
    }
    assert!(ended);
    assert!(fiber.dispose().await.is_clean());
}
#[tokio::test]
async fn provider_retirement_closes_input_even_when_a_reply_waiter_was_dropped() {
    let (fiber, _, duplex) = owners(json!({})).await;
    let managed = duplex
        .spawn(duplex_spec("trap '' TERM; exec /bin/sleep 60", 128))
        .unwrap();
    let stdin = managed.stdin();
    let writing = tokio::spawn(async move {
        let bytes = vec![7; 65536];
        loop {
            if stdin.write(&bytes).await.is_err() {
                break;
            }
        }
    });
    tokio::time::sleep(Duration::from_millis(20)).await;
    writing.abort();
    let _ = writing.await;
    assert!(
        tokio::time::timeout(Duration::from_secs(4), fiber.dispose())
            .await
            .unwrap()
            .is_clean()
    );
    assert!(managed.stdin().write(b"late").await.is_err());
    assert!(
        tokio::time::timeout(Duration::from_millis(100), managed.wait())
            .await
            .is_ok()
    );
}
#[tokio::test]
async fn overlapping_read_rejects_and_cancellation_returns_its_admission() {
    let (fiber, _, duplex) = owners(json!({})).await;
    let managed = duplex.spawn(duplex_spec("exec /bin/cat", 64)).unwrap();
    let output = managed.stdout();
    let mut reading = Box::pin(output.read(64));
    assert!(
        tokio::time::timeout(Duration::from_millis(10), &mut reading)
            .await
            .is_err()
    );
    assert!(matches!(output.read(1).await, Err(ProcessError::Capacity)));
    drop(reading);
    write_all(&managed, b"after cancel").await;
    assert_eq!(output.read(64).await.unwrap().bytes, b"after cancel");
    assert!(output.read(0).await.is_err());
    assert!(output.read(65537).await.is_err());
    assert!(managed.stdin().write(&[]).await.is_err());
    assert!(managed.stdin().write(&vec![1; 65537]).await.is_err());
    managed.stdin().close().await.unwrap();
    managed.wait().await.unwrap();
    assert!(fiber.dispose().await.is_clean());
}

#[tokio::test]
async fn stderr_drain_timeout_is_reported_by_wait_independently_of_stdout_eof() {
    let (fiber, _, duplex) = owners(json!({})).await;
    // The bounded escaped child retains stderr after the direct child closes stdout.
    let script = r#"exec /usr/bin/python3 -c 'import subprocess; subprocess.Popen(["/bin/sleep", "2"], start_new_session=True, stdout=subprocess.DEVNULL)'"#;
    let mut spec = duplex_spec(script, 128);
    spec.termination_grace_ms = 50;
    let managed = duplex.spawn(spec).unwrap();
    let outcome = tokio::time::timeout(Duration::from_secs(1), managed.wait()).await;
    assert!(
        matches!(outcome, Ok(Err(ProcessError::Io(_)))),
        "{outcome:?}"
    );
    managed.wait_settlement().await.unwrap();
    assert!(managed.stdout().read(128).await.unwrap().eof);
    assert!(fiber.dispose().await.is_clean());
}

#[tokio::test]
async fn dropping_the_last_duplex_handle_terminates_while_retained_ports_remain_readable() {
    let (fiber, batch, duplex) = owners(json!({"maximum_active_processes":1})).await;
    let process = duplex
        .spawn(duplex_spec(
            "read word; printf \"%s\" \"$word\"; exec /bin/sleep 60",
            128,
        ))
        .unwrap();
    let kept = process.clone();
    let output = process.stdout();
    drop(process);
    write_all(&kept, b"still owned\n").await;
    assert_eq!(output.read(128).await.unwrap().bytes, b"still owned");
    drop(kept);
    let settled = tokio::time::timeout(Duration::from_secs(3), output.read(128)).await;
    assert!(
        matches!(
            settled,
            Ok(Ok(rsi_process::DuplexRead { eof: true, .. }) | Err(_))
        ),
        "last handle did not close the process: {settled:?}"
    );
    let next = tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            match batch.spawn(spec("exit 0", 1)) {
                Ok(next) => break next,
                // EOF precedes full reaping; the last handle is intentionally gone,
                // so observe admission with bounded backoff instead of a busy loop.
                Err(ProcessError::Capacity) => tokio::time::sleep(Duration::from_millis(5)).await,
                Err(error) => panic!("unexpected spawn failure: {error}"),
            }
        }
    })
    .await
    .unwrap();
    next.wait().await.unwrap();
    drop(next);
    drop(output);
    assert!(fiber.dispose().await.is_clean());
}
