use super::*;
use rsi_sandbox::{
    ConfinedProcess, EnforcementStamp, ProcessStdio, SandboxBackend, SandboxFileSystem,
    SandboxMode, SandboxNetwork, SandboxScratch,
};
fn spec(path: &std::path::Path) -> PtyProcessSpec {
    let root = std::env::var_os("SystemRoot").unwrap();
    PtyProcessSpec {
        process: ConfinedProcess {
            owner: None,
            stdio: ProcessStdio::Pty,
            program: std::path::PathBuf::from(&root)
                .join("System32/WindowsPowerShell/v1.0/powershell.exe")
                .canonicalize()
                .unwrap(),
            arguments: [
                "-NoLogo",
                "-NoProfile",
                "-NoExit",
                "-Command",
                "Remove-Module PSReadLine -ErrorAction SilentlyContinue",
            ]
            .map(Into::into)
            .into(),
            cwd: path.canonicalize().unwrap(),
            stamp: EnforcementStamp {
                requested: SandboxMode::DangerFullAccess,
                backend: SandboxBackend::Unconfined,
                workspace: path.to_owned(),
                filesystem: SandboxFileSystem::Unconfined,
                scratch: SandboxScratch::Host,
                network: SandboxNetwork::Host,
            },
        },
        environment: vec![
            ("SystemRoot".into(), root),
            ("RSI_PTY_TEST".into(), "exact".into()),
        ],
        size: PtySize {
            rows: 24,
            columns: 80,
        },
        termination_grace_ms: 100,
    }
}
async fn receipt(path: &std::path::Path, pty: &ManagedPtyProcess) -> String {
    let mut output = Vec::new();
    let result = tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            if let Ok(value) = tokio::fs::read_to_string(path).await
                && !value.is_empty()
            {
                return value;
            }
            tokio::select! {
                () = tokio::time::sleep(Duration::from_millis(20)) => {},
                page = pty.read() => {
                    let page = page.unwrap();
                    output.extend(page.bytes);
                    assert!(output.len() < 1024 * 1024);
                    assert!(!page.eof, "shell exited before receipt: {:?}", String::from_utf8_lossy(&output));
                }
            }
        }
    })
    .await;
    result.unwrap_or_else(|_| {
        panic!(
            "interactive command receipt: {:?}",
            String::from_utf8_lossy(&output)
        )
    })
}
async fn write(pty: &ManagedPtyProcess, text: &str) {
    let mut bytes = text.as_bytes();
    while !bytes.is_empty() {
        let count = pty.write(bytes).await.unwrap();
        assert!(count > 0);
        bytes = &bytes[count..];
    }
}
async fn prompt(pty: &ManagedPtyProcess) {
    let mut bytes = Vec::new();
    let result = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let page = pty.read().await.unwrap();
            bytes.extend(page.bytes);
            assert!(!page.eof);
            assert!(bytes.len() < 65536);
            if String::from_utf8_lossy(&bytes).contains('>') {
                break;
            }
        }
    })
    .await;
    assert!(
        result.is_ok(),
        "PowerShell prompt over ConPTY: {:?}",
        String::from_utf8_lossy(&bytes)
    );
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn conpty_interactive_unicode_resize_and_exit_preserve_exact_plan() {
    let directory = tempfile::tempdir().unwrap();
    let workspace = directory.path().join("space 工作区");
    std::fs::create_dir(&workspace).unwrap();
    let service = Service::new(crate::ProcessLocalConfig::default());
    let pty = service.spawn_pty(spec(&workspace)).await.unwrap();
    prompt(&pty).await;
    pty.resize(PtySize {
        rows: 31,
        columns: 104,
    })
    .await
    .unwrap();
    write(&pty, "$r=@{cwd=$pwd.Path;cols=$Host.UI.RawUI.WindowSize.Width;rows=$Host.UI.RawUI.WindowSize.Height;redirected=[Console]::IsInputRedirected;value=$env:RSI_PTY_TEST;text='中文'};[IO.File]::WriteAllText((Join-Path $pwd 'receipt.json'),($r|ConvertTo-Json));Write-Output ('RSI'+'-DONE')\r").await;
    let value: serde_json::Value =
        serde_json::from_str(&receipt(&workspace.join("receipt.json"), &pty).await).unwrap();
    assert_eq!(
        std::path::Path::new(value["cwd"].as_str().unwrap())
            .canonicalize()
            .unwrap(),
        workspace.canonicalize().unwrap()
    );
    assert_eq!(value["cols"], 104);
    assert_eq!(value["rows"], 31);
    assert_eq!(value["redirected"], false);
    assert_eq!(value["value"], "exact");
    assert_eq!(value["text"], "中文");
    let reading = pty.clone();
    let output = tokio::spawn(async move {
        let mut data = Vec::new();
        loop {
            let page = reading.read().await.unwrap();
            data.extend(page.bytes);
            assert!(data.len() < 1024 * 1024);
            if page.eof {
                return data;
            }
        }
    });
    write(&pty, "Write-Output ('RSI'+'-EXIT');exit 7\r").await;
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(10), pty.wait())
            .await
            .unwrap()
            .unwrap()
            .exit_code,
        Some(7)
    );
    let bytes = output.await.unwrap();
    assert!(String::from_utf8_lossy(&bytes).contains("RSI-EXIT"));
    service.shutdown().await.unwrap();
    assert!(service.ptys.lock().unwrap().is_empty());
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn conpty_ctrl_c_interrupts_foreground_command_without_terminating_shell() {
    let directory = tempfile::tempdir().unwrap();
    let service = Service::new(crate::ProcessLocalConfig::default());
    let pty = service.spawn_pty(spec(directory.path())).await.unwrap();
    prompt(&pty).await;
    write(&pty,"try{[IO.File]::WriteAllText((Join-Path $pwd 'sleep.txt'),'started');Start-Sleep -Seconds 60}finally{[IO.File]::WriteAllText((Join-Path $pwd 'interrupt.txt'),'interrupted')}\r").await;
    receipt(&directory.path().join("sleep.txt"), &pty).await;
    write(&pty, "\u{3}").await;
    assert_eq!(
        receipt(&directory.path().join("interrupt.txt"), &pty).await,
        "interrupted"
    );
    assert!(
        tokio::time::timeout(Duration::from_millis(20), pty.wait())
            .await
            .is_err()
    );
    pty.terminate();
    pty.wait().await.unwrap();
    service.shutdown().await.unwrap();
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn conpty_retirement_kills_descendants_with_backpressure_and_cancelled_waiter() {
    let directory = tempfile::tempdir().unwrap();
    let service = Service::new(crate::ProcessLocalConfig {
        maximum_active_processes: 1,
        ..Default::default()
    });
    let pty = service.spawn_pty(spec(directory.path())).await.unwrap();
    prompt(&pty).await;
    assert!(matches!(
        service.spawn_pty(spec(directory.path())).await,
        Err(ProcessError::Capacity)
    ));
    let shell = spec(directory.path())
        .process
        .program
        .to_string_lossy()
        .replace('\'', "''");
    write(&pty,&format!("$c=Start-Process -WindowStyle Hidden -PassThru -FilePath '{shell}' -ArgumentList '-NoProfile','-Command','Start-Sleep -Seconds 60';[IO.File]::WriteAllText((Join-Path $pwd 'child.txt'),[string]$c.Id);while($true){{[Console]::Write(('x'*8192))}}\r")).await;
    receipt(&directory.path().join("child.txt"), &pty).await;
    let state = service
        .ptys
        .lock()
        .unwrap()
        .values()
        .next()
        .unwrap()
        .clone();
    assert!(state.job.active().unwrap() >= 2);
    assert!(
        tokio::time::timeout(Duration::from_millis(20), pty.wait())
            .await
            .is_err()
    );
    service.shutdown().await.unwrap();
    pty.wait().await.unwrap();
    assert_eq!(state.job.active().unwrap(), 0);
    assert!(service.ptys.lock().unwrap().is_empty());
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn conpty_spawn_failure_and_final_owner_drop_release_admission() {
    let directory = tempfile::tempdir().unwrap();
    let service = Service::new(crate::ProcessLocalConfig {
        maximum_active_processes: 1,
        ..Default::default()
    });
    let mut invalid = spec(directory.path());
    invalid.process.program = directory.path().join("absent.exe");
    assert!(service.spawn_pty(invalid).await.is_err());
    let pty = service.spawn_pty(spec(directory.path())).await.unwrap();
    let state = service
        .ptys
        .lock()
        .unwrap()
        .values()
        .next()
        .unwrap()
        .clone();
    drop(pty);
    tokio::time::timeout(Duration::from_secs(10), state.wait())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(state.job.active().unwrap(), 0);
    assert_eq!(service.active.available_permits(), 1);
    service.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn conpty_rejects_verbatim_paths_that_would_retarget_a_different_directory() {
    let directory = tempfile::tempdir().unwrap();
    let normal = directory.path().join("distinct");
    std::fs::create_dir(&normal).unwrap();
    let special = directory.path().canonicalize().unwrap().join("distinct.");
    std::fs::create_dir(&special).unwrap();
    let service = Service::new(crate::ProcessLocalConfig::default());
    assert!(service.spawn_pty(spec(&special)).await.is_err());
    assert_eq!(service.active.available_permits(), service.maximum_active);
    service.shutdown().await.unwrap();
}
