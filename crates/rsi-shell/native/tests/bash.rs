#![cfg(target_os = "linux")]

use async_trait::async_trait;
use rsi_jobs::JobStatus;
use rsi_sandbox::{Sandbox, SandboxMode};
use rsi_shell_native::{
    MAXIMUM_SHELL_COMMAND_BYTES, MAXIMUM_SHELL_TIMEOUT_MS, ShellKind, ShellToolFactory,
};
use rsi_tools_protocol::{
    ToolCall, ToolContent, ToolExecutionExtensions, ToolExecutionPolicy, ToolStart,
};
use serde_json::json;
use std::{ffi::OsString, path::PathBuf, sync::Arc};
use tokio_util::sync::CancellationToken;

mod common;
use common::Fixture;

#[tokio::test]
async fn restricted_bash_fails_closed_without_a_verified_sandbox_backend() {
    let fixture = Fixture::activate().await;
    let marker = fixture.workspace.path().join("restricted-marker");

    assert!(
        fixture
            .call_with_mode(
                json!({"command":"printf blocked > restricted-marker"}),
                SandboxMode::WorkspaceWrite,
            )
            .await
            .is_err()
    );
    assert!(!marker.exists());
    fixture.shutdown().await;
}

#[tokio::test]
async fn foreground_preview_reads_bounded_native_output_before_completion_without_reporting() {
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
    let fixture = Fixture::activate().await;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let command = format!(
        "exec 3<>/dev/tcp/127.0.0.1/{port}; head -c 40000 /dev/zero | tr '\\0' x; printf ready >&3; IFS= read -r -u 3; printf done; printf err >&2"
    );
    let call = fixture.call(json!({"command":command,"timeout_ms":10000}));
    let probe = async {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut ready = [0; 5];
        socket.read_exact(&mut ready).await.unwrap();
        assert_eq!(&ready, b"ready");
        let job = fixture
            .jobs
            .list(&fixture.scope)
            .unwrap()
            .into_iter()
            .next()
            .unwrap();
        assert_eq!(job.status, JobStatus::Running);
        assert!(job.requires_report);
        let origin = job.origin.as_deref().unwrap();
        assert!(
            fixture
                .jobs
                .peek(&fixture.scope, &job.id, "another-effect", [32, 32])
                .is_err()
        );
        assert!(
            fixture
                .jobs
                .peek(&fixture.scope, &job.id, origin, [32769, 32])
                .is_err()
        );
        tokio::time::timeout(std::time::Duration::from_secs(3), async {
            loop {
                let page = fixture
                    .jobs
                    .peek(&fixture.scope, &job.id, origin, [31, 17])
                    .unwrap()
                    .unwrap();
                assert!(page.stdout.bytes.len() <= 31);
                assert!(page.stderr.bytes.len() <= 17);
                if page.stdout.next_offset == 40000 {
                    assert_eq!(page.stdout.bytes, b"xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx");
                    assert_eq!(page.stdout.oldest_offset, 39969);
                    assert!(page.stdout.lossy);
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert_eq!(fixture.jobs.get(&fixture.scope, &job.id).unwrap(), job);
        socket.write_all(b"continue\n").await.unwrap();
        (job.id, origin.to_owned())
    };
    let (result, (id, origin)) = tokio::time::timeout(std::time::Duration::from_secs(15), async {
        tokio::join!(call, probe)
    })
    .await
    .unwrap();
    let result = result.unwrap();
    assert!(!result.is_error);
    assert!(fixture.jobs.get(&fixture.scope, &id).unwrap().reported);
    assert!(
        fixture
            .jobs
            .peek(&fixture.scope, &id, &origin, [32, 32])
            .unwrap()
            .is_none()
    );
    assert!(
        fixture
            .jobs
            .finalize_scope(&fixture.scope)
            .await
            .unwrap()
            .unreported
            .is_empty()
    );
    assert!(
        fixture
            .jobs
            .peek(&fixture.scope, &id, &origin, [32, 32])
            .is_err()
    );
    fixture.shutdown().await;
}

#[test]
fn shell_factory_defers_file_admission_and_rejects_duplicate_retained_names() {
    assert!(
        ShellToolFactory::new(
            ShellKind::Bash,
            PathBuf::from("/definitely/missing/rsi-bash"),
            Vec::new(),
        )
        .is_ok()
    );
    assert!(
        ShellToolFactory::new(
            ShellKind::Bash,
            PathBuf::from("/bin/bash"),
            vec![
                (OsString::from("VISIBLE"), OsString::from("first")),
                (OsString::from("VISIBLE"), OsString::from("second")),
            ],
        )
        .is_err()
    );
}

#[tokio::test]
async fn factories_publish_only_bash_and_preserve_foreground_and_background_behavior() {
    let fixture = Fixture::activate().await;
    let definitions = fixture.tools.definitions();
    assert_eq!(
        definitions
            .iter()
            .map(rsi_tools_protocol::ToolDefinition::name)
            .collect::<Vec<_>>(),
        ["bash"]
    );
    assert_eq!(
        definitions[0].description(),
        "Run an exact Bash command. Foreground commands wait for the complete process group; use run_in_background for long-lived work. Nonzero exits, signals, and command timeout are normal outcomes."
    );
    assert_eq!(
        definitions[0].input_schema(),
        &json!({
            "type":"object",
            "properties":{
                "command":{"type":"string","maxLength":MAXIMUM_SHELL_COMMAND_BYTES},
                "timeout_ms":{"type":"integer","minimum":1,"maximum":MAXIMUM_SHELL_TIMEOUT_MS},
                "run_in_background":{"type":"boolean"}
            },
            "required":["command"],
            "additionalProperties":false
        })
    );

    let foreground = fixture
        .call(json!({
            "command":"printf '%s|%s|%s|%s|%s' \"$VISIBLE\" \"$NO_COLOR\" \"$TERM\" \"$PAGER\" \"$GIT_PAGER\"; printf warning >&2; exit 7"
        }))
        .await
        .unwrap();
    assert!(!foreground.is_error);
    assert_eq!(foreground.value["status"], "exited");
    assert_eq!(foreground.value["exit_code"], 7);
    assert_eq!(foreground.value["stdout"]["text"], "yes|1|dumb|cat|cat");
    assert_eq!(foreground.value["stderr"]["text"], "warning");
    assert!(
        matches!(foreground.content.as_slice(), [ToolContent::Text { text }]
        if text.contains("[status: exited; exit code: 7; signal: none]"))
    );

    let truncated = fixture
        .call(json!({
            "command":"head -c 65537 /dev/zero | tr '\\0' x"
        }))
        .await
        .unwrap();
    assert_eq!(truncated.value["stdout"]["truncated"], true);
    assert!(matches!(
        truncated.content.as_slice(),
        [ToolContent::Text { text }] if text.starts_with("[stdout truncated; showing retained tail]\n")
    ));

    let accepted = fixture
        .call(json!({"command":"#".repeat(MAXIMUM_SHELL_COMMAND_BYTES)}))
        .await
        .unwrap();
    assert!(!accepted.is_error);
    let rejected = fixture
        .call(json!({"command":"#".repeat(MAXIMUM_SHELL_COMMAND_BYTES + 1)}))
        .await
        .unwrap();
    assert!(rejected.is_error);
    assert_eq!(rejected.value["code"], "invalid_arguments");
    assert!(rejected.enforcement.is_empty());

    let timed_out = fixture
        .call(json!({"command":"sleep 1","timeout_ms":10}))
        .await
        .unwrap();
    assert!(!timed_out.is_error);
    assert_eq!(timed_out.value["status"], "timed_out");
    assert!(timed_out.value["signal"].as_i64().is_some());
    assert!(
        matches!(timed_out.content.as_slice(), [ToolContent::Text { text }]
        if text.contains("status: timed_out") && text.contains("signal:"))
    );

    let background_timeout = fixture
        .call(json!({
            "command":"true",
            "timeout_ms":10,
            "run_in_background":true
        }))
        .await
        .unwrap();
    assert!(background_timeout.is_error);
    assert_eq!(background_timeout.value["code"], "invalid_arguments");
    assert!(background_timeout.enforcement.is_empty());

    let started = fixture
        .call(json!({
            "command":"printf begin; sleep 0.02; printf end; printf warning >&2",
            "run_in_background":true
        }))
        .await
        .unwrap();
    let id = started.value["job_id"].as_str().unwrap();
    let settled = fixture.jobs.wait(&fixture.scope, id, 0, 0).await.unwrap();
    assert_eq!(settled.job.producer, "rsi.coding.bash");
    assert_eq!(settled.job.status, JobStatus::Completed);
    assert_eq!(settled.stdout.bytes, b"beginend");
    assert_eq!(settled.stderr.bytes, b"warning");
    fixture.shutdown().await;
}

#[tokio::test]
async fn background_nonzero_exit_is_a_failed_job_terminal() {
    let fixture = Fixture::activate().await;
    let started = fixture
        .call(json!({"command":format!(":\n# {}\nexit 7", "测".repeat(100)), "run_in_background":true}))
        .await
        .unwrap();
    let id = started.value["job_id"].as_str().unwrap();
    let failed = fixture.jobs.wait(&fixture.scope, id, 0, 0).await.unwrap();
    assert_eq!(failed.job.name, format!(": # {}…", "测".repeat(83)));

    assert_eq!(failed.job.status, JobStatus::Failed);
    assert_eq!(failed.job.terminal.unwrap().exit_code, Some(7));
    fixture.shutdown().await;
}

#[tokio::test]
async fn foreground_output_sanitizes_terminal_controls_only_in_model_text() {
    let fixture = Fixture::activate().await;
    let result = fixture
        .call(json!({"command":"printf '\\033[31mred\\007\\177'"}))
        .await
        .unwrap();

    assert_eq!(
        result.value["stdout"]["text"], "\u{1b}[31mred\u{7}\u{7f}",
        "the structured stream remains the authoritative lossy UTF-8 projection"
    );
    assert!(matches!(
        result.content.as_slice(),
        [ToolContent::Text { text }] if text == "\u{fffd}[31mred\u{fffd}\u{fffd}\n[status: exited; exit code: 0; signal: none]"
    ));
    fixture.shutdown().await;
}

#[path = "bash/target.rs"]
mod target;
