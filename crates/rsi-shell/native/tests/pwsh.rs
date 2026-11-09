#![cfg(windows)]

mod common;

use common::Fixture;
use rsi_jobs::JobStatus;
use rsi_sandbox::SandboxMode;
use rsi_shell_native::ShellKind;
use serde_json::json;
use std::path::PathBuf;

async fn fixture(program: PathBuf) -> Fixture {
    let mut environment: Vec<_> = std::env::vars_os().collect();
    environment.push(("RSI_SHELL_TEST_SECRET".into(), "must-not-leak".into()));
    Fixture::activate_with(ShellKind::PowerShell, program, environment).await
}

fn system_powershell() -> PathBuf {
    PathBuf::from(std::env::var_os("SystemRoot").unwrap())
        .join("System32/WindowsPowerShell/v1.0/powershell.exe")
}

#[tokio::test]
async fn native_pwsh_catalog_preserves_utf8_exit_status_and_environment() {
    let fixture = fixture(system_powershell()).await;
    let definitions = fixture.tools.definitions();
    assert_eq!(definitions.len(), 1);
    assert_eq!(definitions[0].name(), "pwsh");
    assert!(definitions[0].description().contains("PowerShell syntax"));
    let output = fixture.call(json!({"command": r#"[Console]::Write("中文|$env:NO_COLOR|$env:TERM|$env:PAGER|$env:GIT_PAGER|$env:RSI_SHELL_TEST_SECRET"); [Console]::Error.Write('warning'); exit 7"#})).await.unwrap();
    assert!(!output.is_error);
    assert_eq!(output.value["status"], "exited");
    assert_eq!(output.value["exit_code"], 7);
    assert_eq!(output.value["stdout"]["text"], "中文|1|dumb|cat|cat|");
    assert_eq!(output.value["stdout"]["utf8_lossy"], false);
    assert_eq!(output.value["stderr"]["text"], "warning");
    fixture.shutdown().await;
}

#[tokio::test]
async fn native_pwsh_jobs_preserve_failure_timeout_and_cancellation() {
    let fixture = fixture(system_powershell()).await;
    let started = fixture
        .call(json!({"command":"[Console]::Write('background'); exit 7", "run_in_background":true}))
        .await
        .unwrap();
    let id = started.value["job_id"].as_str().unwrap();
    let failed = fixture.jobs.wait(&fixture.scope, id, 0, 0).await.unwrap();
    assert_eq!(failed.job.producer, "rsi.coding.pwsh");
    assert_eq!(failed.job.status, JobStatus::Failed);
    assert_eq!(failed.job.terminal.unwrap().exit_code, Some(7));
    assert_eq!(failed.stdout.bytes, b"background");

    let started = fixture
        .call(json!({"command":"Start-Sleep -Seconds 60", "run_in_background":true}))
        .await
        .unwrap();
    let id = started.value["job_id"].as_str().unwrap();
    let killed = fixture.jobs.kill(&fixture.scope, id).await.unwrap();
    assert_eq!(killed.job.status, JobStatus::Cancelled);
    let timed_out = fixture
        .call(json!({"command":"Start-Sleep -Seconds 60", "timeout_ms":2000}))
        .await
        .unwrap();
    assert!(!timed_out.is_error);
    assert_eq!(timed_out.value["status"], "timed_out");
    assert_eq!(timed_out.value["signal"], serde_json::Value::Null);
    fixture.shutdown().await;
}

#[tokio::test]
#[ignore = "opt-in native restricted-token and workspace ACL mutation; RSI_SHELL_TEST_PROGRAM selects PowerShell 7 or Store alias"]
async fn restricted_pwsh_executes_through_sealed_tool_runtime() {
    let program =
        std::env::var_os("RSI_SHELL_TEST_PROGRAM").map_or_else(system_powershell, PathBuf::from);
    let fixture = fixture(program).await;
    let read = fixture.call_with_mode(json!({"command": r#"[Console]::Write("中文|$($ExecutionContext.SessionState.LanguageMode)"); [System.IO.File]::WriteAllText((Join-Path $env:TEMP 'scratch.txt'), 'ok'); exit 7"#}), SandboxMode::ReadOnly).await.unwrap();
    assert_eq!(read.value["exit_code"], 7);
    assert_eq!(read.value["stdout"]["text"], "中文|FullLanguage");
    assert!(!read.enforcement.is_empty());
    let write = fixture.call_with_mode(json!({"command": "[System.IO.File]::WriteAllText((Join-Path (Get-Location) 'native.txt'), '中文', [System.Text.UTF8Encoding]::new($false))"}), SandboxMode::WorkspaceWrite).await.unwrap();
    assert_eq!(write.value["exit_code"], 0);
    assert_eq!(
        std::fs::read_to_string(fixture.workspace.path().join("native.txt")).unwrap(),
        "中文"
    );
    fixture.shutdown().await;
}
