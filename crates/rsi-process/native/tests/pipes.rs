#![cfg(windows)]

use rsi_process_native::{Job, PipeChild, PipeSpawn};
use std::{ffi::OsString, path::PathBuf, time::Duration};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

struct Fixture {
    directory: tempfile::TempDir,
    program: PathBuf,
    arguments: Vec<OsString>,
    environment: Vec<(OsString, OsString)>,
}

impl Fixture {
    fn new(script: &str) -> Self {
        let directory = tempfile::tempdir().unwrap();
        let file = directory.path().join("child.ps1");
        std::fs::write(&file, script).unwrap();
        let root = std::env::var_os("SystemRoot").unwrap();
        Self {
            directory,
            program: PathBuf::from(&root).join(r"System32\WindowsPowerShell\v1.0\powershell.exe"),
            arguments: [
                "-NoProfile",
                "-NonInteractive",
                "-ExecutionPolicy",
                "Bypass",
                "-File",
            ]
            .into_iter()
            .map(OsString::from)
            .chain([file.into_os_string()])
            .collect(),
            environment: vec![
                ("SystemRoot".into(), root),
                ("RSI_PIPE_FIXTURE".into(), "exact".into()),
            ],
        }
    }

    async fn spawn(&self) -> PipeChild {
        PipeChild::spawn(PipeSpawn {
            program: &self.program,
            arguments: &self.arguments,
            environment: &self.environment,
            cwd: self.directory.path(),
            token: None,
        })
        .await
        .unwrap()
    }
}

#[test]
fn pipes_and_wait_progress_with_the_only_blocking_worker_occupied() {
    let mut fixture = Fixture::new(
        r"
[Console]::OutputEncoding = [Text.UTF8Encoding]::new($false)
[IO.File]::WriteAllText((Join-Path $pwd 'started'), 'yes')
$receipt = @{ arguments = @($args); cwd = $pwd.Path; environment = $env:RSI_PIPE_FIXTURE; absent = $env:RSI_PIPE_ABSENT }
[Console]::Error.Write(($receipt | ConvertTo-Json -Compress))
[Console]::OpenStandardInput().CopyTo([Console]::OpenStandardOutput())
exit 7
",
    );
    fixture.arguments.extend(
        ["with spaces", "with\"quote", "trailing\\", "汉字", ""]
            .into_iter()
            .map(OsString::from),
    );
    // Compare the actual PowerShell argv semantics with the previous launcher.
    let baseline = std::process::Command::new(&fixture.program)
        .args(&fixture.arguments)
        .env_clear()
        .envs(fixture.environment.iter().cloned())
        .current_dir(fixture.directory.path())
        .output()
        .unwrap();
    assert_eq!(baseline.status.code(), Some(7));
    let receipt: serde_json::Value = serde_json::from_slice(&baseline.stderr).unwrap();
    assert_eq!(receipt["environment"], "exact");
    assert!(receipt["absent"].is_null());
    std::fs::remove_file(fixture.directory.path().join("started")).unwrap();

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .max_blocking_threads(1)
        .build()
        .unwrap();
    let (release, blocked) = std::sync::mpsc::channel::<()>();
    let (ready, started) = std::sync::mpsc::channel();
    let worker = runtime.spawn_blocking(move || {
        ready.send(()).unwrap();
        let _ = blocked.recv();
    });
    started.recv().unwrap();
    runtime.block_on(async {
        let job = Job::new().unwrap();
        let mut child = fixture.spawn().await;
        assert!(child.try_wait().unwrap().is_none());
        assert!(!fixture.directory.path().join("started").exists());
        child.resume(&job).unwrap();
        assert_eq!(job.active().unwrap(), 1);
        let mut stdin = child.stdin.take().unwrap();
        let mut stdout = child.stdout.take().unwrap();
        let mut stderr = child.stderr.take().unwrap();
        let bytes: Vec<_> = (0..512 * 1024)
            .map(|n| u8::try_from(n % 251).unwrap())
            .collect();
        let mut received = Vec::new();
        let mut diagnostic = Vec::new();
        let status = tokio::time::timeout(Duration::from_secs(15), async {
            let ((), _, _, status) = tokio::try_join!(
                async {
                    stdin.write_all(&bytes).await?;
                    drop(stdin);
                    Ok::<_, std::io::Error>(())
                },
                stdout.read_to_end(&mut received),
                stderr.read_to_end(&mut diagnostic),
                child.wait(),
            )?;
            Ok::<_, std::io::Error>(status)
        })
        .await
        .expect("pipe I/O and exit waiting must not borrow the occupied blocking worker")
        .unwrap();
        assert_eq!(status.code(), Some(7));
        assert_eq!(received, bytes);
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&diagnostic).unwrap(),
            receipt
        );
        assert_eq!(child.try_wait().unwrap(), Some(status));
        assert_eq!(job.active().unwrap(), 0);
    });
    drop(release);
    runtime.block_on(worker).unwrap();
}

#[tokio::test]
async fn cancelling_a_wait_retains_the_child_and_exit_259_is_not_pending() {
    let fixture = Fixture::new("[Console]::OpenStandardInput().ReadByte() | Out-Null; exit 259");
    let job = Job::new().unwrap();
    let mut child = fixture.spawn().await;
    child.resume(&job).unwrap();
    for _ in 0..8 {
        assert!(
            tokio::time::timeout(Duration::from_millis(2), child.wait())
                .await
                .is_err()
        );
        assert!(child.try_wait().unwrap().is_none());
    }
    child.stdin.take().unwrap().write_all(b"x").await.unwrap();
    let status = tokio::time::timeout(Duration::from_secs(10), child.wait())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(status.code(), Some(259));
    assert_eq!(child.try_wait().unwrap(), Some(status));
    assert_eq!(job.active().unwrap(), 0);
}

#[tokio::test]
async fn dropping_a_suspended_child_does_not_execute_user_code() {
    let fixture = Fixture::new("[IO.File]::WriteAllText((Join-Path $pwd 'started'), 'yes')");
    let child = fixture.spawn().await;
    let handle = rsi_process_native::ProcessHandle::open(child.id()).unwrap();
    drop(child);
    assert!(!fixture.directory.path().join("started").exists());
    assert!(!handle.is_running().unwrap());
}

#[tokio::test]
#[allow(unsafe_code)]
async fn an_unrelated_inheritable_handle_is_excluded_from_the_child() {
    use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
    use windows_sys::Win32::{
        Foundation::{DUPLICATE_SAME_ACCESS, DuplicateHandle},
        System::Threading::GetCurrentProcess,
    };
    let mut fixture = Fixture::new(
        r#"
$ErrorActionPreference = 'Stop'
Add-Type -TypeDefinition 'using System; using System.Text; using System.Runtime.InteropServices;
public static class PipeHandleProbe {
[DllImport("kernel32.dll", CharSet=CharSet.Unicode)]
public static extern uint GetFinalPathNameByHandleW(IntPtr h, StringBuilder b, uint n, uint f);
}'
$buffer = [Text.StringBuilder]::new(1024)
[void][PipeHandleProbe]::GetFinalPathNameByHandleW([IntPtr]::new([long]$env:RSI_EXTRA_HANDLE), $buffer, 1024, 0)
[Console]::Write($buffer.ToString())
"#,
    );
    let secret = fixture.directory.path().join("unrelated-private-file");
    for name in ["TEMP", "TMP"] {
        fixture
            .environment
            .push((name.into(), fixture.directory.path().as_os_str().to_owned()));
    }
    let file = std::fs::File::create(&secret).unwrap();
    let mut duplicate = std::ptr::null_mut();
    // SAFETY: The live file is duplicated within this process. Success transfers
    // exactly one inheritable handle, retained until the child has been reaped.
    assert_ne!(
        unsafe {
            DuplicateHandle(
                GetCurrentProcess(),
                file.as_raw_handle(),
                GetCurrentProcess(),
                &raw mut duplicate,
                0,
                1,
                DUPLICATE_SAME_ACCESS,
            )
        },
        0
    );
    // SAFETY: The successful duplicate is owned here exactly once.
    let inherited = unsafe { OwnedHandle::from_raw_handle(duplicate) };
    fixture.environment.push((
        "RSI_EXTRA_HANDLE".into(),
        format!("{}", duplicate as usize).into(),
    ));
    let job = Job::new().unwrap();
    let mut child = fixture.spawn().await;
    child.resume(&job).unwrap();
    drop(child.stdin.take());
    let mut stdout = child.stdout.take().unwrap();
    let mut stderr = child.stderr.take().unwrap();
    let mut output = Vec::new();
    let mut error = Vec::new();
    let (_, _, status) = tokio::time::timeout(Duration::from_secs(15), async {
        tokio::try_join!(
            stdout.read_to_end(&mut output),
            stderr.read_to_end(&mut error),
            child.wait()
        )
    })
    .await
    .unwrap()
    .unwrap();
    assert_eq!(
        status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&error)
    );
    assert!(error.is_empty(), "{}", String::from_utf8_lossy(&error));
    assert!(!String::from_utf8_lossy(&output).contains("unrelated-private-file"));
    drop(inherited);
    assert_eq!(job.active().unwrap(), 0);
}
