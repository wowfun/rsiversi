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
                // Pin ConsoleHost's cooked reader rather than image-specific PSReadLine hooks.
                "Remove-Module PSReadLine -ErrorAction SilentlyContinue; function global:PSConsoleHostReadLine { $Host.UI.ReadLine() }",
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
            "interactive command receipt: {:?}; progress: {:?}",
            String::from_utf8_lossy(&output),
            std::fs::read_to_string(path.with_extension("progress.json"))
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
    eprintln!("PowerShell startup: {:?}", String::from_utf8_lossy(&bytes));
    assert!(
        result.is_ok(),
        "PowerShell prompt over ConPTY: {:?}",
        String::from_utf8_lossy(&bytes)
    );
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn conpty_input_reaches_native_console_key_events() {
    native_key_events("a中\r").await;
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn conpty_win32_input_reaches_native_console_key_events() {
    native_key_events("\u{1b}[65;30;97;1;0;1_\u{1b}[231;0;20013;1;0;1_\u{1b}[13;28;13;1;0;1_")
        .await;
}
fn literal(path: &std::path::Path) -> String {
    format!("'{}'", path.to_str().unwrap().replace('\'', "''"))
}
async fn native_key_events(input: &str) {
    let directory = tempfile::tempdir().unwrap();
    let service = Service::new(crate::ProcessLocalConfig::default());
    let mut plan = spec(directory.path());
    let path = directory.path().join("keys.json");
    let script = format!(
        "[Console]::Write('KEYS-READY>');$keys=@();for($i=0;$i-lt 3;$i++){{$k=[Console]::ReadKey($true);$keys+=('[{{0}},{{1}},{{2}}]' -f [int]$k.KeyChar,[int]$k.Key,[int]$k.Modifiers);[IO.File]::WriteAllText({},('['+($keys -join ',')+']'))}};[IO.File]::Move({},{})",
        literal(&path.with_extension("progress.json")),
        literal(&path.with_extension("progress.json")),
        literal(&path),
    );
    plan.process.arguments = ["-NoLogo", "-NoProfile", "-Command", &script]
        .map(Into::into)
        .into();
    let pty = service.spawn_pty(plan).await.unwrap();
    prompt(&pty).await;
    write(&pty, input).await;
    let value: serde_json::Value = serde_json::from_str(&receipt(&path, &pty).await).unwrap();
    assert_eq!(value[0][0], 97, "{value}");
    assert_eq!(value[1][0], u32::from('中'), "{value}");
    assert_eq!(value[2][0], 13, "{value}");
    assert_eq!(value[2][1], 13, "{value}");
    assert_eq!(value[2][2], 0, "{value}");
    pty.wait().await.unwrap();
    service.shutdown().await.unwrap();
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
    let path = workspace.join("receipt.txt");
    write(&pty, &format!("[IO.File]::WriteAllLines({},[string[]]@($pwd.Path,$Host.UI.RawUI.WindowSize.Width,$Host.UI.RawUI.WindowSize.Height,[Console]::IsInputRedirected,$env:RSI_PTY_TEST,'中文'));[Console]::WriteLine('RSI'+'-DONE')\r",literal(&path))).await;
    let receipt = receipt(&path, &pty).await;
    let value = receipt.lines().collect::<Vec<_>>();
    assert_eq!(value.len(), 6, "{receipt}");
    assert_eq!(
        std::path::Path::new(value[0]).canonicalize().unwrap(),
        workspace.canonicalize().unwrap()
    );
    assert_eq!(value[1], "104");
    assert_eq!(value[2], "31");
    assert_eq!(value[3], "False");
    assert_eq!(value[4], "exact");
    assert_eq!(value[5], "中文");
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
    write(&pty, "[Console]::WriteLine('RSI'+'-EXIT');exit 7\r").await;
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
    write(&pty,&format!("try{{[IO.File]::WriteAllText({},'started');while($true){{[Threading.Thread]::Sleep(10)}}}}finally{{[IO.File]::WriteAllText({},'interrupted')}}\r",literal(&directory.path().join("sleep.txt")),literal(&directory.path().join("interrupt.txt")))).await;
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
    write(&pty,&format!("$s=[Diagnostics.ProcessStartInfo]::new('{shell}','-NoProfile -Command [Threading.Thread]::Sleep(60000)');$s.UseShellExecute=$false;$s.CreateNoWindow=$true;$c=[Diagnostics.Process]::Start($s);[IO.File]::WriteAllText({},[string]$c.Id);while($true){{[Console]::Write(('x'*8192))}}\r",literal(&directory.path().join("child.txt")))).await;
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
