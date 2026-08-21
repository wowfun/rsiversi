use std::collections::VecDeque;
use std::num::NonZeroU8;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};
use std::time::Duration;

use async_trait::async_trait;
use rsi_agent_protocol::{
    ToolResult as WireToolResult, ToolsCatalogResponse, ToolsInvokeRequest, ToolsInvokeResponse,
};
use serde_json::json;
use tempfile::tempdir;
use tokio::sync::{Semaphore, oneshot};

use crate::adapter::{
    CommittedModelRequest, ModelPort, PortBundle, PortError, PortFactory, PreparedModelCall,
    ToolPort, ValidatedAssistantMessage, ValidatedToolCatalog, ValidatedToolResponse,
};
use crate::persistence::Store;
use crate::{
    AgentError, AgentHost, AgentWorkspace, AiOperationId, ExecutionLimits, Failure, FailureKind,
    RunRequest, RunStatus, SessionId, ToolOutcome, TranscriptEventKind,
};

#[allow(dead_code)]
#[derive(Clone, Debug)]
struct ModelCompleteRequest {
    system_prompt: String,
    messages: Vec<ModelMessage>,
    tools: Vec<rsi_agent_protocol::ToolDefinition>,
}

#[allow(dead_code)]
#[derive(Clone, Debug)]
enum ModelMessage {
    User {
        content: String,
    },
    Assistant {
        content: Option<String>,
        tool_calls: Vec<LegacyToolCall>,
    },
    Tool {
        call_id: String,
        result: WireToolResult,
    },
}

#[allow(dead_code)]
#[derive(Clone, Debug)]
struct LegacyToolCall {
    id: String,
    name: String,
    arguments: String,
}

#[test]
fn execution_limits_have_stable_defaults_and_reject_invalid_deadlines() {
    let defaults = ExecutionLimits::default();
    assert_eq!(defaults.handshake_timeout(), Duration::from_secs(10));
    assert_eq!(defaults.model_response_timeout(), Duration::from_mins(1));
    assert_eq!(defaults.tool_response_timeout(), Duration::from_secs(30));
    assert_eq!(defaults.provider_turn_timeout(), Duration::from_mins(5));

    assert!(matches!(
        ExecutionLimits::new(
            Duration::ZERO,
            Duration::from_secs(1),
            Duration::from_secs(1),
            Duration::from_secs(2),
        ),
        Err(AgentError::InvalidInput {
            field: "execution_limits",
            ..
        })
    ));
    assert!(matches!(
        ExecutionLimits::new(
            Duration::from_secs(1),
            Duration::from_secs(3),
            Duration::from_secs(1),
            Duration::from_secs(2),
        ),
        Err(AgentError::InvalidInput {
            field: "execution_limits",
            ..
        })
    ));
    assert!(matches!(
        ExecutionLimits::new(
            Duration::from_secs(1),
            Duration::from_secs(1),
            Duration::from_secs(1),
            Duration::MAX,
        ),
        Err(AgentError::InvalidInput {
            field: "execution_limits",
            ..
        })
    ));
}

#[tokio::test(start_paused = true)]
async fn accepted_execution_limits_remain_panic_free_after_the_clock_advances() {
    let now = std::time::Instant::now();
    let mut lower = 0_u64;
    let mut upper = u64::MAX;
    while lower < upper {
        let candidate = lower + (upper - lower) / 2 + 1;
        if now.checked_add(Duration::from_secs(candidate)).is_some() {
            lower = candidate;
        } else {
            upper = candidate - 1;
        }
    }
    let timeout = Duration::from_secs(lower.saturating_sub(60));
    let limits = ExecutionLimits::new(timeout, timeout, timeout, timeout)
        .expect("deadline is representable when the host opens");
    let temp = tempdir().expect("tempdir");
    let state = FakeState::shared(vec![final_message("must not run")]);
    let host = AgentHost::open_with_factory_and_limits(
        AgentWorkspace::new(temp.path().join("agent")),
        Box::new(FakeFactory::new(Arc::clone(&state))),
        limits,
    )
    .await
    .expect("host");

    tokio::time::advance(Duration::from_mins(2)).await;
    let id = SessionId::new("aged-deadline").expect("id");
    let record = host
        .run(RunRequest::new(id.clone(), "do not panic after the clock advances").expect("request"))
        .await
        .expect("deadline overflow closes durably");

    assert!(matches!(
        record.status(),
        RunStatus::Failed {
            failure: Failure {
                kind: FailureKind::TimedOut,
                ..
            }
        }
    ));
    assert_eq!(state.lock().expect("state").model_requests.len(), 0);
    assert!(
        host.transcript(&id)
            .await
            .expect("deadline overflow does not poison the host")
            .is_some()
    );
}

#[tokio::test(start_paused = true)]
async fn direct_ai_supervision_rejects_an_aged_unrepresentable_deadline_before_polling() {
    let now = std::time::Instant::now();
    let mut lower = 0_u64;
    let mut upper = u64::MAX;
    while lower < upper {
        let candidate = lower + (upper - lower) / 2 + 1;
        if now.checked_add(Duration::from_secs(candidate)).is_some() {
            lower = candidate;
        } else {
            upper = candidate - 1;
        }
    }
    let timeout = Duration::from_secs(lower.saturating_sub(60));
    let temp = tempdir().expect("tempdir");
    let host = AgentHost::open_with_factory(
        AgentWorkspace::new(temp.path().join("agent")),
        Box::new(FakeFactory::new(FakeState::shared(Vec::new()))),
    )
    .await
    .expect("host");
    tokio::time::advance(Duration::from_mins(2)).await;
    let entered = Arc::new(AtomicBool::new(false));

    let error = host
        .supervise_ai_operation(
            AiOperationId::new("aged-direct-deadline").expect("operation id"),
            timeout,
            {
                let entered = Arc::clone(&entered);
                async move {
                    entered.store(true, Ordering::Release);
                    std::future::pending::<()>().await;
                    Ok(())
                }
            },
        )
        .await
        .expect_err("the aged deadline is no longer representable");

    assert!(matches!(error, AgentError::Ai { .. }));
    assert!(!entered.load(Ordering::Acquire));
}

#[tokio::test(start_paused = true)]
async fn direct_ai_provider_deadline_begins_after_durable_reservation() {
    let temp = tempdir().expect("tempdir");
    let host = AgentHost::open_with_factory(
        AgentWorkspace::new(temp.path().join("agent")),
        Box::new(FakeFactory::new(FakeState::shared(Vec::new()))),
    )
    .await
    .expect("host");
    let gate = host.gate_next_ai_reserve().await.expect("reservation gate");
    let entered = Arc::new(AtomicBool::new(false));
    let operation = tokio::spawn({
        let host = host.clone();
        let entered = Arc::clone(&entered);
        async move {
            host.supervise_ai_operation(
                AiOperationId::new("reservation-does-not-burn-deadline").expect("operation id"),
                Duration::from_secs(1),
                async move {
                    entered.store(true, Ordering::Release);
                    Ok(7_u8)
                },
            )
            .await
        }
    });
    gate.entered().await;
    tokio::time::advance(Duration::from_secs(2)).await;
    assert!(!entered.load(Ordering::Acquire));
    gate.release();

    assert_eq!(
        operation.await.expect("supervisor task").expect("result"),
        7
    );
    assert!(entered.load(Ordering::Acquire));
}

#[tokio::test(start_paused = true)]
async fn direct_ai_provider_deadline_aborts_work_and_closes_its_reservation() {
    let temp = tempdir().expect("tempdir");
    let workspace = temp.path().join("agent");
    let host = AgentHost::open_with_factory(
        AgentWorkspace::new(&workspace),
        Box::new(FakeFactory::new(FakeState::shared(Vec::new()))),
    )
    .await
    .expect("host");
    let operation_id = AiOperationId::new("direct-deadline").expect("operation id");
    let entered = Arc::new(tokio::sync::Notify::new());
    let (drop_signal, dropped) = oneshot::channel::<()>();
    let operation = tokio::spawn({
        let host = host.clone();
        let operation_id = operation_id.clone();
        let entered = Arc::clone(&entered);
        async move {
            host.supervise_ai_operation(operation_id, Duration::from_secs(5), async move {
                let _drop_signal = drop_signal;
                entered.notify_one();
                std::future::pending::<crate::Result<()>>().await
            })
            .await
        }
    });
    entered.notified().await;
    tokio::time::advance(Duration::from_secs(5)).await;

    let error = operation
        .await
        .expect("supervisor task")
        .expect_err("provider deadline");
    assert!(matches!(
        error,
        AgentError::Ai {
            operation: "execute AI operation",
            ..
        }
    ));
    dropped
        .await
        .expect_err("aborted operation dropped its sender");
    let connection = rusqlite::Connection::open(workspace.join("agent.sqlite3"))
        .expect("inspect durable operation");
    let (phase, terminal): (i64, String) = connection
        .query_row(
            "SELECT phase, terminal_status FROM ai_operations WHERE operation_id=?1",
            [operation_id.as_str()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("operation row");
    assert_eq!((phase, terminal.as_str()), (3, "not_started"));
}

#[tokio::test(start_paused = true)]
async fn custom_execution_limits_drive_provider_deadlines() {
    let temp = tempdir().expect("tempdir");
    let state = FakeState::shared(Vec::new());
    state.lock().expect("state").model_pending = true;
    let limits = ExecutionLimits::new(
        Duration::from_secs(2),
        Duration::from_secs(7),
        Duration::from_secs(3),
        Duration::from_secs(20),
    )
    .expect("limits");
    let host = AgentHost::open_with_factory_and_limits(
        AgentWorkspace::new(temp.path().join("agent")),
        Box::new(FakeFactory::new(state)),
        limits,
    )
    .await
    .expect("host");

    let started = tokio::time::Instant::now();
    let record = host
        .run(
            RunRequest::new(
                SessionId::new("custom-model-timeout").expect("id"),
                "wait for the model",
            )
            .expect("request"),
        )
        .await
        .expect("durable timeout");

    assert_eq!(
        tokio::time::Instant::now() - started,
        Duration::from_secs(7)
    );
    assert!(matches!(
        record.status(),
        RunStatus::Failed {
            failure: Failure {
                kind: FailureKind::TimedOut,
                ..
            }
        }
    ));
}

#[tokio::test(start_paused = true)]
async fn expired_provider_turn_stops_new_effects_but_still_closes_durably() {
    let temp = tempdir().expect("tempdir");
    let state = FakeState::shared(vec![call_message(vec![(
        "expired-tool",
        json!({"text":"must not run"}).to_string(),
    )])]);
    let limits = ExecutionLimits::new(
        Duration::from_secs(2),
        Duration::from_secs(4),
        Duration::from_secs(3),
        Duration::from_secs(5),
    )
    .expect("limits");
    let host = AgentHost::open_with_factory_and_limits(
        AgentWorkspace::new(temp.path().join("agent")),
        Box::new(FakeFactory::new(Arc::clone(&state))),
        limits,
    )
    .await
    .expect("host");
    let gate = host
        .gate_next_dispatch_commit()
        .await
        .expect("dispatch gate");
    let run = tokio::spawn({
        let host = host.clone();
        async move {
            host.run(
                RunRequest::new(
                    SessionId::new("expired-effect-admission").expect("id"),
                    "wait past the provider deadline",
                )
                .expect("request"),
            )
            .await
        }
    });

    gate.entered().await;
    tokio::time::advance(Duration::from_secs(6)).await;
    gate.release();
    let record = run.await.expect("run task").expect("durable outcome");

    assert!(matches!(
        record.status(),
        RunStatus::Failed {
            failure: Failure {
                kind: FailureKind::TimedOut,
                ..
            }
        }
    ));
    assert!(state.lock().expect("state").invocations.is_empty());
}

#[tokio::test]
async fn direct_final_is_durable_and_same_identity_is_effect_free() {
    let temp = tempdir().expect("tempdir");
    let state = FakeState::shared(vec![final_message("done")]);
    let host = AgentHost::open_with_factory(
        AgentWorkspace::new(temp.path().join("agent")),
        Box::new(FakeFactory::new(Arc::clone(&state))),
    )
    .await
    .expect("host");
    let id = SessionId::new("direct-final").expect("id");
    let first = host
        .run(RunRequest::new(id.clone(), "answer directly").expect("request"))
        .await
        .expect("run");
    assert_eq!(
        first.status(),
        &RunStatus::Completed {
            final_message: "done".to_owned()
        }
    );
    let replay = host
        .run(RunRequest::new(id.clone(), "answer directly").expect("request"))
        .await
        .expect("replay");
    assert_eq!(first, replay);
    assert_eq!(host.commit_count(), 5, "direct completion commit budget");
    {
        let snapshot = state.lock().expect("state");
        assert_eq!(snapshot.opens, 1);
        assert_eq!(snapshot.model_requests.len(), 1);
        assert!(snapshot.invocations.is_empty());
    }
    assert!(matches!(
        host.run(RunRequest::new(id, "different").expect("request"))
            .await,
        Err(AgentError::SessionConflict { .. })
    ));

    let model_bound_id = SessionId::new("direct-final-model").expect("id");
    host.run(
        RunRequest::new(model_bound_id.clone(), "same prompt")
            .expect("request")
            .with_model("model-a")
            .expect("model"),
    )
    .await
    .expect("model-bound run");
    assert!(matches!(
        host.run(
            RunRequest::new(model_bound_id, "same prompt")
                .expect("request")
                .with_model("model-b")
                .expect("model"),
        )
        .await,
        Err(AgentError::SessionConflict { .. })
    ));
}

#[tokio::test]
async fn scripted_model_output_preserves_sources_and_warnings() {
    let temp = tempdir().expect("tempdir");
    let mut output = final_message("grounded");
    output.sources.push(rsi_ai_protocol::Source {
        id: "source-1".to_owned(),
        title: Some("Primary source".to_owned()),
        url: Some("https://example.com/evidence".to_owned()),
    });
    output.warnings.push(rsi_ai_protocol::Warning {
        code: "provider_notice".to_owned(),
        message: "bounded warning".to_owned(),
    });
    let state = FakeState::shared(vec![output.clone()]);
    let host = AgentHost::open_with_factory(
        AgentWorkspace::new(temp.path().join("agent")),
        Box::new(FakeFactory::new(state)),
    )
    .await
    .expect("host");
    let id = SessionId::new("source-warning-output").expect("id");
    host.run(RunRequest::new(id.clone(), "answer with evidence").expect("request"))
        .await
        .expect("run");
    let transcript = host
        .transcript(&id)
        .await
        .expect("transcript")
        .expect("present");
    let message = transcript
        .events()
        .iter()
        .find_map(|event| match event.kind() {
            TranscriptEventKind::AssistantMessage { message } => Some(message),
            _ => None,
        })
        .expect("assistant message");
    assert_eq!(message.sources, output.sources);
    assert_eq!(message.warnings, output.warnings);
}

#[tokio::test(start_paused = true)]
async fn transient_model_failures_retry_only_after_durable_retry_events() {
    let temp = tempdir().expect("tempdir");
    let state = FakeState::shared(vec![final_message("recovered")]);
    {
        let mut state = state.lock().expect("state");
        state.model_failures.extend((0..2).map(|_| {
            rsi_ai_protocol::AiError::new(
                rsi_ai_protocol::ErrorKind::RateLimited,
                rsi_ai_protocol::ErrorPhase::FirstEvent,
                rsi_ai_protocol::DispatchStatus::Dispatched,
                "scripted rate limit",
            )
            .expect("error")
            .with_retry_after_ms(1)
        }));
    }
    let host = AgentHost::open_with_factory(
        AgentWorkspace::new(temp.path().join("agent")),
        Box::new(FakeFactory::new(Arc::clone(&state))),
    )
    .await
    .expect("host");
    let id = SessionId::new("durable-model-retry").expect("id");

    let record = host
        .run(RunRequest::new(id.clone(), "retry safely").expect("request"))
        .await
        .expect("run");

    assert_eq!(
        record.status(),
        &RunStatus::Completed {
            final_message: "recovered".to_owned()
        }
    );
    assert_eq!(state.lock().expect("state").model_requests.len(), 3);
    let transcript = host
        .transcript(&id)
        .await
        .expect("transcript")
        .expect("present");
    let retries = transcript
        .events()
        .iter()
        .filter_map(|event| match event.kind() {
            TranscriptEventKind::ModelRetryScheduled {
                failed_attempt,
                delay_ms,
                ..
            } => Some((*failed_attempt, *delay_ms)),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(retries, [(1, 1), (2, 1)]);
    assert_eq!(host.commit_count(), 9, "every retry boundary is committed");
}

#[tokio::test]
async fn durable_prompt_wins_when_wrong_probe_candidate_arrives_first() {
    let temp = tempdir().expect("tempdir");
    let state = FakeState::shared(vec![final_message("durable")]);
    let host = AgentHost::open_with_factory(
        AgentWorkspace::new(temp.path().join("agent")),
        Box::new(FakeFactory::new(Arc::clone(&state))),
    )
    .await
    .expect("host");
    let id = SessionId::new("probe-prompt-order").expect("id");
    let durable = host
        .run(RunRequest::new(id.clone(), "prompt-b").expect("request"))
        .await
        .expect("seed terminal session");

    let gate = host.gate_next_probe().expect("probe gate");
    let wrong = tokio::spawn({
        let host = host.clone();
        let id = id.clone();
        async move {
            host.run(RunRequest::new(id, "prompt-a").expect("request"))
                .await
        }
    });
    gate.entered().await;

    let (accepted, observed) = oneshot::channel();
    let correct = tokio::spawn({
        let host = host.clone();
        let id = id.clone();
        async move {
            host.run_with_acceptance_signal(
                RunRequest::new(id, "prompt-b").expect("request"),
                accepted,
            )
            .await
        }
    });
    observed
        .await
        .expect("correct candidate reached coordinator");
    gate.release();

    assert!(matches!(
        wrong.await.expect("wrong task"),
        Err(AgentError::SessionConflict { .. })
    ));
    assert_eq!(
        correct.await.expect("correct task").expect("replay"),
        durable
    );
    assert_eq!(state.lock().expect("state").opens, 1);
}

#[tokio::test]
async fn admission_store_failure_requires_recovery_without_service_work() {
    let temp = tempdir().expect("tempdir");
    let state = FakeState::shared(vec![final_message("must not run")]);
    let host = AgentHost::open_with_factory(
        AgentWorkspace::new(temp.path().join("agent")),
        Box::new(FakeFactory::new(Arc::clone(&state))),
    )
    .await
    .expect("host");
    host.make_writes_fail().await.expect("query-only mode");
    let id = SessionId::new("admission-failure").expect("id");
    let error = host
        .run(RunRequest::new(id.clone(), "persist first").expect("request"))
        .await
        .expect_err("admission must fail");
    assert!(matches!(
        error,
        AgentError::RecoveryRequired { session_id, .. } if session_id == id
    ));
    let snapshot = state.lock().expect("state");
    assert_eq!(snapshot.opens, 0);
    assert!(snapshot.model_requests.is_empty());
}

#[tokio::test]
#[allow(clippy::too_many_lines)] // Keep the poison, barrier, and reopen chronology in one test.
async fn uncertain_dispatch_poisons_all_active_work_and_reopen_recovers_without_effects() {
    let temp = tempdir().expect("tempdir");
    let workspace = AgentWorkspace::new(temp.path().join("agent"));
    let state = FakeState::shared(vec![
        call_message(vec![(
            "blocked-call",
            json!({"text":"blocked"}).to_string(),
        )]),
        call_message(vec![(
            "uncertain-call",
            json!({"text":"uncertain"}).to_string(),
        )]),
    ]);
    let model_gate = Arc::new(ModelGate::default());
    state.lock().expect("state").model_gate = Some(Arc::clone(&model_gate));
    let host = AgentHost::open_with_factory(
        workspace.clone(),
        Box::new(FakeFactory::new(Arc::clone(&state))),
    )
    .await
    .expect("host");

    let blocked_id = SessionId::new("poison-blocked").expect("id");
    let blocked_prompt = "wait at the model barrier";
    let blocked = tokio::spawn({
        let host = host.clone();
        let blocked_id = blocked_id.clone();
        async move {
            host.run(RunRequest::new(blocked_id, blocked_prompt).expect("request"))
                .await
        }
    });
    model_gate.wait_until_entered().await;

    state.lock().expect("state").model_gate = None;
    host.fail_next_dispatch_commit_uncertain()
        .await
        .expect("arm uncertain dispatch");
    let uncertain_id = SessionId::new("poison-trigger").expect("id");
    let uncertain_prompt = "commit a dispatch without an acknowledgement";
    let trigger = host
        .run(RunRequest::new(uncertain_id.clone(), uncertain_prompt).expect("trigger request"))
        .await
        .expect_err("uncertain commit requires recovery");
    assert!(matches!(
        trigger,
        AgentError::RecoveryRequired { session_id, .. } if session_id == uncertain_id
    ));
    assert!(state.lock().expect("state").invocations.is_empty());

    let rejected_id = SessionId::new("poison-rejected").expect("id");
    assert!(matches!(
        host.run(RunRequest::new(rejected_id, "must not start").expect("request"))
            .await,
        Err(AgentError::HostTerminal)
    ));

    model_gate.release(1);
    let blocked_error = blocked
        .await
        .expect("blocked task")
        .expect_err("the poisoned host stops at the next durable barrier");
    assert!(matches!(
        blocked_error,
        AgentError::RecoveryRequired { session_id, .. } if session_id == blocked_id
    ));
    assert!(state.lock().expect("state").invocations.is_empty());
    drop(host);

    let reopened_state = FakeState::shared(Vec::new());
    let reopened = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            match AgentHost::open_with_factory(
                workspace.clone(),
                Box::new(FakeFactory::new(Arc::clone(&reopened_state))),
            )
            .await
            {
                Err(AgentError::WorkspaceOccupied { .. }) => tokio::task::yield_now().await,
                result => return result,
            }
        }
    })
    .await
    .expect("old workers release the workspace")
    .expect("reopen recovers interrupted sessions");

    let uncertain = reopened
        .run(RunRequest::new(uncertain_id.clone(), uncertain_prompt).expect("uncertain replay"))
        .await
        .expect("uncertain replay");
    let blocked = reopened
        .run(RunRequest::new(blocked_id.clone(), blocked_prompt).expect("blocked replay"))
        .await
        .expect("blocked replay");
    assert_eq!(uncertain.status(), &RunStatus::Interrupted);
    assert_eq!(blocked.status(), &RunStatus::Interrupted);

    let uncertain_transcript = reopened
        .transcript(&uncertain_id)
        .await
        .expect("uncertain transcript")
        .expect("uncertain session exists");
    assert!(uncertain_transcript.events().iter().any(|event| matches!(
        event.kind(),
        TranscriptEventKind::ToolResult {
            outcome: ToolOutcome::OutcomeUnknown,
            ..
        }
    )));
    let blocked_transcript = reopened
        .transcript(&blocked_id)
        .await
        .expect("blocked transcript")
        .expect("blocked session exists");
    assert!(!blocked_transcript.events().iter().any(|event| matches!(
        event.kind(),
        TranscriptEventKind::ToolDispatchStarted { .. }
    )));
    let reopened_snapshot = reopened_state.lock().expect("reopened state");
    assert_eq!(reopened_snapshot.opens, 0);
    assert!(reopened_snapshot.model_requests.is_empty());
    assert!(reopened_snapshot.invocations.is_empty());
}

#[tokio::test]
async fn dropping_the_run_future_does_not_cancel_admitted_work() {
    let temp = tempdir().expect("tempdir");
    let workspace = temp.path().join("agent");
    let state = FakeState::shared(vec![final_message("durable")]);
    let gate = Arc::new(ModelGate::default());
    state.lock().expect("state").model_gate = Some(Arc::clone(&gate));
    let host = AgentHost::open_with_factory(
        AgentWorkspace::new(&workspace),
        Box::new(FakeFactory::new(state)),
    )
    .await
    .expect("host");
    let id = SessionId::new("dropped-future").expect("id");
    let task = tokio::spawn({
        let id = id.clone();
        async move {
            let host = host;
            host.run(RunRequest::new(id, "finish without caller").expect("request"))
                .await
        }
    });
    gate.wait_until_entered().await;
    task.abort();
    assert!(task.await.expect_err("task was aborted").is_cancelled());
    gate.release(1);

    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let connection =
                rusqlite::Connection::open(workspace.join("agent.sqlite3")).expect("read store");
            let status = connection
                .query_row(
                    "SELECT terminal FROM sessions WHERE session_id=?1",
                    [id.as_str()],
                    |row| row.get::<_, i64>(0),
                )
                .expect("session");
            if status == 1 {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("actor completed admitted run");
    let store = Store::open(&workspace.join("agent.sqlite3")).expect("reopen store");
    let transcript = store.transcript(&id).expect("transcript").expect("present");
    assert_eq!(
        transcript.status(),
        &RunStatus::Completed {
            final_message: "durable".to_owned()
        }
    );
}

#[tokio::test]
async fn dropping_a_direct_ai_caller_does_not_cancel_supervised_work() {
    let temp = tempdir().expect("tempdir");
    let state = FakeState::shared(Vec::new());
    let host = AgentHost::open_with_factory(
        AgentWorkspace::new(temp.path().join("agent")),
        Box::new(FakeFactory::new(state)),
    )
    .await
    .expect("host");
    let entered = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let completed = Arc::new(AtomicBool::new(false));
    let caller = tokio::spawn({
        let host = host.clone();
        let entered = Arc::clone(&entered);
        let release = Arc::clone(&release);
        let completed = Arc::clone(&completed);
        async move {
            host.supervise_ai_operation(
                AiOperationId::new("supervised-drop").expect("operation id"),
                Duration::from_secs(2),
                async move {
                    entered.notify_one();
                    release.notified().await;
                    completed.store(true, Ordering::Release);
                    Err::<(), _>(AgentError::Ai {
                        operation: "test supervised operation",
                        message: "expected terminal test error".to_owned(),
                    })
                },
            )
            .await
        }
    });
    entered.notified().await;
    caller.abort();
    assert!(caller.await.expect_err("caller abort").is_cancelled());
    release.notify_one();

    tokio::time::timeout(Duration::from_secs(1), async {
        while !completed.load(Ordering::Acquire) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("supervised operation completes without its caller");
}

#[tokio::test]
async fn ordinary_direct_ai_errors_terminalize_the_owned_reservation() {
    let temp = tempdir().expect("tempdir");
    let workspace = temp.path().join("agent");
    let host = AgentHost::open_with_factory(
        AgentWorkspace::new(&workspace),
        Box::new(FakeFactory::new(FakeState::shared(Vec::new()))),
    )
    .await
    .expect("host");
    let operation_id = AiOperationId::new("ordinary-direct-error").expect("operation id");

    let error = host
        .supervise_ai_operation(operation_id.clone(), Duration::from_secs(2), async {
            Err::<(), _>(AgentError::Ai {
                operation: "open image service",
                message: "service is unbound".to_owned(),
            })
        })
        .await
        .expect_err("ordinary operation error");
    assert!(matches!(error, AgentError::Ai { .. }));

    let connection = rusqlite::Connection::open(workspace.join("agent.sqlite3"))
        .expect("inspect durable operation");
    let (phase, terminal): (i64, String) = connection
        .query_row(
            "SELECT phase, terminal_status FROM ai_operations WHERE operation_id=?1",
            [operation_id.as_str()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("operation row");
    assert_eq!((phase, terminal.as_str()), (3, "not_started"));
}

#[tokio::test]
async fn durable_abandonment_failure_is_not_masked_as_a_provider_error() {
    let temp = tempdir().expect("tempdir");
    let host = AgentHost::open_with_factory(
        AgentWorkspace::new(temp.path().join("agent")),
        Box::new(FakeFactory::new(FakeState::shared(Vec::new()))),
    )
    .await
    .expect("host");
    let entered = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let operation = tokio::spawn({
        let host = host.clone();
        let entered = Arc::clone(&entered);
        let release = Arc::clone(&release);
        async move {
            host.supervise_ai_operation(
                AiOperationId::new("abandonment-storage-failure").expect("operation id"),
                Duration::from_secs(2),
                async move {
                    entered.notify_one();
                    release.notified().await;
                    Err::<(), _>(AgentError::Ai {
                        operation: "test provider operation",
                        message: "provider failed".to_owned(),
                    })
                },
            )
            .await
        }
    });
    entered.notified().await;
    host.make_writes_fail().await.expect("query-only mode");
    release.notify_one();

    let error = operation
        .await
        .expect("supervisor task")
        .expect_err("abandonment must fail");
    assert!(matches!(error, AgentError::Persistence { .. }));
    assert!(matches!(
        host.transcript(&SessionId::new("after-abandonment-failure").expect("id"))
            .await,
        Err(AgentError::HostTerminal)
    ));
}

#[tokio::test]
async fn independent_sessions_execute_concurrently() {
    let temp = tempdir().expect("tempdir");
    let state = FakeState::shared(vec![final_message("one"), final_message("two")]);
    let gate = Arc::new(ModelGate::default());
    state.lock().expect("state").model_gate = Some(Arc::clone(&gate));
    let host = AgentHost::open_with_factory_and_concurrency(
        AgentWorkspace::new(temp.path().join("agent")),
        Box::new(FakeFactory::new(Arc::clone(&state))),
        NonZeroU8::new(2).expect("nonzero"),
    )
    .await
    .expect("host");

    let first = tokio::spawn({
        let host = host.clone();
        async move {
            host.run(
                RunRequest::new(SessionId::new("parallel-one").expect("id"), "first prompt")
                    .expect("request"),
            )
            .await
        }
    });
    let second = tokio::spawn({
        let host = host.clone();
        async move {
            host.run(
                RunRequest::new(SessionId::new("parallel-two").expect("id"), "second prompt")
                    .expect("request"),
            )
            .await
        }
    });
    tokio::time::timeout(Duration::from_secs(1), async {
        gate.wait_until_entered().await;
        gate.wait_until_entered().await;
    })
    .await
    .expect("both model calls entered before either was released");
    gate.release(2);

    assert!(matches!(
        first
            .await
            .expect("first task")
            .expect("first run")
            .status(),
        RunStatus::Completed { .. }
    ));
    assert!(matches!(
        second
            .await
            .expect("second task")
            .expect("second run")
            .status(),
        RunStatus::Completed { .. }
    ));
    assert_eq!(state.lock().expect("state").opens, 2);
}

#[tokio::test]
async fn concurrent_same_identity_joins_one_execution() {
    let temp = tempdir().expect("tempdir");
    let state = FakeState::shared(vec![final_message("shared")]);
    let gate = Arc::new(ModelGate::default());
    state.lock().expect("state").model_gate = Some(Arc::clone(&gate));
    let host = AgentHost::open_with_factory(
        AgentWorkspace::new(temp.path().join("agent")),
        Box::new(FakeFactory::new(Arc::clone(&state))),
    )
    .await
    .expect("host");
    let id = SessionId::new("same-id-join").expect("id");
    let first = tokio::spawn({
        let host = host.clone();
        let id = id.clone();
        async move {
            host.run(RunRequest::new(id, "same prompt").expect("request"))
                .await
        }
    });
    gate.wait_until_entered().await;
    let (accepted, observed) = oneshot::channel();
    let second = tokio::spawn({
        let host = host.clone();
        let id = id.clone();
        async move {
            host.run_with_acceptance_signal(
                RunRequest::new(id, "same prompt").expect("request"),
                accepted,
            )
            .await
        }
    });
    observed.await.expect("joiner reached coordinator");
    gate.release(1);

    let first = first.await.expect("first task").expect("first run");
    let second = second.await.expect("second task").expect("joined run");
    assert_eq!(first, second);
    let snapshot = state.lock().expect("state");
    assert_eq!(snapshot.opens, 1);
    assert_eq!(snapshot.model_requests.len(), 1);
}

#[tokio::test]
async fn concurrent_same_identity_and_prompt_rejects_a_different_model() {
    let temp = tempdir().expect("tempdir");
    let state = FakeState::shared(vec![final_message("shared")]);
    let gate = Arc::new(ModelGate::default());
    state.lock().expect("state").model_gate = Some(Arc::clone(&gate));
    let host = AgentHost::open_with_factory(
        AgentWorkspace::new(temp.path().join("agent")),
        Box::new(FakeFactory::new(Arc::clone(&state))),
    )
    .await
    .expect("host");
    let id = SessionId::new("same-id-different-model").expect("id");
    let first = tokio::spawn({
        let host = host.clone();
        let id = id.clone();
        async move {
            host.run(
                RunRequest::new(id, "same prompt")
                    .expect("request")
                    .with_model("model-a")
                    .expect("model"),
            )
            .await
        }
    });
    gate.wait_until_entered().await;
    let conflict = host
        .run(
            RunRequest::new(id, "same prompt")
                .expect("request")
                .with_model("model-b")
                .expect("model"),
        )
        .await;
    assert!(matches!(conflict, Err(AgentError::SessionConflict { .. })));
    gate.release(1);
    first.await.expect("first task").expect("first run");
    assert_eq!(state.lock().expect("state").opens, 1);
}

#[tokio::test]
async fn default_ninth_session_waits_for_an_execution_slot() {
    let temp = tempdir().expect("tempdir");
    let state = FakeState::shared(
        (0..9)
            .map(|index| final_message(&format!("done-{index}")))
            .collect(),
    );
    let gate = Arc::new(ModelGate::default());
    state.lock().expect("state").model_gate = Some(Arc::clone(&gate));
    let host = AgentHost::open_with_factory(
        AgentWorkspace::new(temp.path().join("agent")),
        Box::new(FakeFactory::new(Arc::clone(&state))),
    )
    .await
    .expect("host");

    let mut runs = Vec::new();
    for index in 0..8 {
        runs.push(tokio::spawn({
            let host = host.clone();
            async move {
                host.run(
                    RunRequest::new(
                        SessionId::new(format!("default-slot-{index}")).expect("session id"),
                        format!("prompt-{index}"),
                    )
                    .expect("request"),
                )
                .await
            }
        }));
    }
    for _ in 0..8 {
        gate.wait_until_entered().await;
    }

    let (accepted, observed) = oneshot::channel();
    let ninth = tokio::spawn({
        let host = host.clone();
        async move {
            host.run_with_acceptance_signal(
                RunRequest::new(
                    SessionId::new("default-slot-8").expect("session id"),
                    "prompt-8",
                )
                .expect("request"),
                accepted,
            )
            .await
        }
    });
    observed.await.expect("ninth call reached coordinator");
    tokio::task::yield_now().await;
    assert!(!ninth.is_finished());
    assert_eq!(state.lock().expect("state").opens, 8);

    gate.release(1);
    tokio::time::timeout(Duration::from_secs(1), gate.wait_until_entered())
        .await
        .expect("ninth session entered after one slot was released");
    assert_eq!(state.lock().expect("state").opens, 9);
    gate.release(8);

    for run in runs {
        assert!(matches!(
            run.await.expect("task").expect("run").status(),
            RunStatus::Completed { .. }
        ));
    }
    assert!(matches!(
        ninth
            .await
            .expect("ninth task")
            .expect("ninth run")
            .status(),
        RunStatus::Completed { .. }
    ));
}

#[tokio::test]
async fn admitted_run_calls_are_bounded_at_four_times_concurrency() {
    let temp = tempdir().expect("tempdir");
    let state = FakeState::shared(vec![final_message("shared")]);
    let gate = Arc::new(ModelGate::default());
    state.lock().expect("state").model_gate = Some(Arc::clone(&gate));
    let host = AgentHost::open_with_factory_and_concurrency(
        AgentWorkspace::new(temp.path().join("agent")),
        Box::new(FakeFactory::new(Arc::clone(&state))),
        NonZeroU8::MIN,
    )
    .await
    .expect("host");
    let id = SessionId::new("bounded-admission").expect("id");
    let first = tokio::spawn({
        let host = host.clone();
        let id = id.clone();
        async move {
            host.run(RunRequest::new(id, "same prompt").expect("request"))
                .await
        }
    });
    gate.wait_until_entered().await;

    let mut joiners = Vec::new();
    for _ in 0..3 {
        let (accepted, observed) = oneshot::channel();
        joiners.push(tokio::spawn({
            let host = host.clone();
            let id = id.clone();
            async move {
                host.run_with_acceptance_signal(
                    RunRequest::new(id, "same prompt").expect("request"),
                    accepted,
                )
                .await
            }
        }));
        observed.await.expect("joiner reached coordinator");
    }

    let (fifth_accepted, mut fifth_observed) = oneshot::channel();
    let fifth = tokio::spawn({
        let host = host.clone();
        let id = id.clone();
        async move {
            host.run_with_acceptance_signal(
                RunRequest::new(id, "same prompt").expect("request"),
                fifth_accepted,
            )
            .await
        }
    });
    assert!(
        tokio::time::timeout(Duration::from_millis(50), &mut fifth_observed)
            .await
            .is_err(),
        "fifth call bypassed the four-call admission bound"
    );

    gate.release(1);
    let first = first.await.expect("first task").expect("first run");
    fifth_observed
        .await
        .expect("fifth call entered after permits were released");
    for joiner in joiners {
        assert_eq!(joiner.await.expect("join task").expect("join run"), first);
    }
    assert_eq!(fifth.await.expect("fifth task").expect("fifth run"), first);
    let snapshot = state.lock().expect("state");
    assert_eq!(snapshot.opens, 1);
    assert_eq!(snapshot.model_requests.len(), 1);
}

#[tokio::test]
async fn slow_session_does_not_block_an_unrelated_terminal_transcript() {
    let temp = tempdir().expect("tempdir");
    let state = FakeState::shared(vec![final_message("seed"), final_message("slow")]);
    let host = AgentHost::open_with_factory(
        AgentWorkspace::new(temp.path().join("agent")),
        Box::new(FakeFactory::new(Arc::clone(&state))),
    )
    .await
    .expect("host");
    let terminal_id = SessionId::new("terminal-while-slow").expect("id");
    host.run(RunRequest::new(terminal_id.clone(), "seed prompt").expect("request"))
        .await
        .expect("seed run");

    let gate = Arc::new(ModelGate::default());
    state.lock().expect("state").model_gate = Some(Arc::clone(&gate));
    let slow = tokio::spawn({
        let host = host.clone();
        async move {
            host.run(
                RunRequest::new(SessionId::new("slow-unrelated").expect("id"), "slow prompt")
                    .expect("request"),
            )
            .await
        }
    });
    gate.wait_until_entered().await;

    let transcript =
        tokio::time::timeout(Duration::from_millis(250), host.transcript(&terminal_id))
            .await
            .expect("unrelated transcript was not serialized behind the slow run")
            .expect("transcript read")
            .expect("terminal transcript");
    assert_eq!(
        transcript.status(),
        &RunStatus::Completed {
            final_message: "seed".to_owned()
        }
    );
    gate.release(1);
    slow.await.expect("slow task").expect("slow run");
}

#[cfg(unix)]
#[tokio::test]
async fn workspace_uses_its_canonical_root_after_an_intermediate_symlink_moves() {
    use std::os::unix::fs::symlink;

    let temp = tempdir().expect("tempdir");
    let first_parent = temp.path().join("first");
    let second_parent = temp.path().join("second");
    std::fs::create_dir(&first_parent).expect("first parent");
    std::fs::create_dir(&second_parent).expect("second parent");
    let link = temp.path().join("current");
    symlink(&first_parent, &link).expect("initial parent link");

    let state = FakeState::shared(vec![final_message("canonical")]);
    let host = AgentHost::open_with_factory(
        AgentWorkspace::new(link.join("agent")),
        Box::new(FakeFactory::new(state)),
    )
    .await
    .expect("host through intermediate symlink");

    std::fs::remove_file(&link).expect("remove old parent link");
    symlink(&second_parent, &link).expect("replacement parent link");
    let id = SessionId::new("canonical-workspace").expect("id");
    host.run(RunRequest::new(id.clone(), "stay with the leased root").expect("request"))
        .await
        .expect("run on leased workspace");

    let transcript = host
        .transcript(&id)
        .await
        .expect("cold read uses the canonical leased root")
        .expect("transcript");
    assert!(matches!(transcript.status(), RunStatus::Completed { .. }));
    assert!(!second_parent.join("agent").exists());
}

#[tokio::test(flavor = "current_thread")]
async fn sqlite_write_contention_does_not_block_the_tokio_thread() {
    let temp = tempdir().expect("tempdir");
    let workspace = AgentWorkspace::new(temp.path().join("agent"));
    let state = FakeState::shared(vec![final_message("done")]);
    let host = AgentHost::open_with_factory(workspace.clone(), Box::new(FakeFactory::new(state)))
        .await
        .expect("host");
    let lock = rusqlite::Connection::open(workspace.database_path()).expect("lock connection");
    lock.execute_batch("BEGIN IMMEDIATE").expect("write lock");

    let (accepted, observed) = oneshot::channel();
    let run = tokio::spawn({
        let host = host.clone();
        async move {
            host.run_with_acceptance_signal(
                RunRequest::new(SessionId::new("sqlite-off-runtime").expect("id"), "hello")
                    .expect("request"),
                accepted,
            )
            .await
        }
    });
    observed.await.expect("run reached coordinator");
    tokio::task::yield_now().await;
    tokio::time::timeout(
        Duration::from_millis(250),
        tokio::time::sleep(Duration::from_millis(25)),
    )
    .await
    .expect("SQLite contention blocked the current-thread runtime");
    assert!(!run.is_finished());

    lock.execute_batch("ROLLBACK").expect("release write lock");
    assert!(matches!(
        run.await.expect("task").expect("run").status(),
        RunStatus::Completed { .. }
    ));
}

#[tokio::test]
async fn cold_terminal_corruption_is_lazy_and_isolated_to_its_session() {
    let temp = tempdir().expect("tempdir");
    let workspace = AgentWorkspace::new(temp.path().join("agent"));
    std::fs::create_dir_all(workspace.root()).expect("workspace");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(workspace.root(), std::fs::Permissions::from_mode(0o700))
            .expect("workspace permissions");
    }
    let corrupt_id = SessionId::new("lazy-corrupt").expect("id");
    let mut store = Store::open(&workspace.database_path()).expect("seed store");
    store
        .begin_session(&corrupt_id, "seed")
        .expect("seed session");
    store
        .append_terminal(
            &corrupt_id,
            &[
                TranscriptEventKind::StepEnded {
                    step: crate::StepId::new(1),
                    outcome: crate::BoundaryOutcome::Interrupted,
                },
                TranscriptEventKind::TurnEnded {
                    outcome: crate::BoundaryOutcome::Interrupted,
                },
            ],
            RunStatus::Interrupted,
        )
        .expect("seed terminal transcript");
    drop(store);
    rusqlite::Connection::open(workspace.database_path())
        .expect("corrupt connection")
        .execute(
            "UPDATE events SET payload_json='{}' WHERE session_id=?1 AND seq=1",
            [corrupt_id.as_str()],
        )
        .expect("corrupt cold terminal event");

    let state = FakeState::shared(vec![final_message("unrelated")]);
    let host =
        AgentHost::open_with_factory(workspace, Box::new(FakeFactory::new(Arc::clone(&state))))
            .await
            .expect("cold terminal corruption does not block open");
    let unrelated_id = SessionId::new("lazy-corrupt-unrelated").expect("id");
    host.run(RunRequest::new(unrelated_id.clone(), "unrelated").expect("request"))
        .await
        .expect("unrelated run remains usable");

    assert!(matches!(
        host.transcript(&corrupt_id).await,
        Err(AgentError::CorruptSession { session_id, .. }) if session_id == corrupt_id
    ));
    assert!(
        host.transcript(&unrelated_id)
            .await
            .expect("unrelated transcript remains readable")
            .is_some()
    );
}

#[tokio::test]
async fn corrupt_cold_session_does_not_stop_an_active_dispatch() {
    let temp = tempdir().expect("tempdir");
    let workspace = AgentWorkspace::new(temp.path().join("agent"));
    let state = FakeState::shared(vec![
        final_message("seed"),
        call_message(vec![("still-runs", json!({"text":"allowed"}).to_string())]),
        final_message("active completed"),
    ]);
    let host = AgentHost::open_with_factory(
        workspace.clone(),
        Box::new(FakeFactory::new(Arc::clone(&state))),
    )
    .await
    .expect("host");
    let corrupt_id = SessionId::new("poison-source").expect("id");
    host.run(RunRequest::new(corrupt_id.clone(), "seed").expect("request"))
        .await
        .expect("seed terminal session");
    rusqlite::Connection::open(workspace.database_path())
        .expect("corrupt connection")
        .execute(
            "UPDATE events SET payload_json='{}' WHERE session_id=?1 AND seq=1",
            [corrupt_id.as_str()],
        )
        .expect("corrupt terminal event");

    let gate = host
        .gate_next_dispatch_commit()
        .await
        .expect("dispatch commit gate");
    let active_id = SessionId::new("poisoned-dispatch").expect("id");
    let active = tokio::spawn({
        let host = host.clone();
        let active_id = active_id.clone();
        async move {
            host.run(RunRequest::new(active_id, "use echo").expect("request"))
                .await
        }
    });
    gate.entered().await;
    assert!(state.lock().expect("state").invocations.is_empty());

    assert!(matches!(
        host.transcript(&corrupt_id).await,
        Err(AgentError::CorruptSession { session_id, .. }) if session_id == corrupt_id
    ));
    gate.release();
    assert!(matches!(
        active.await.expect("active task").expect("active run").status(),
        RunStatus::Completed { final_message } if final_message == "active completed"
    ));
    assert_eq!(state.lock().expect("state").invocations, ["still-runs"]);
}

#[tokio::test]
async fn multiple_tools_are_serial_and_second_request_is_log_projection() {
    let temp = tempdir().expect("tempdir");
    let state = FakeState::shared(vec![
        call_message(vec![
            ("call-1", json!({"text":"one"}).to_string()),
            ("call-2", json!({"text":"two"}).to_string()),
        ]),
        final_message("complete"),
    ]);
    let host = AgentHost::open_with_factory(
        AgentWorkspace::new(temp.path().join("agent")),
        Box::new(FakeFactory::new(Arc::clone(&state))),
    )
    .await
    .expect("host");
    let id = SessionId::new("two-tools").expect("id");
    let record = host
        .run(RunRequest::new(id.clone(), "echo twice").expect("request"))
        .await
        .expect("run");
    assert!(matches!(record.status(), RunStatus::Completed { .. }));

    {
        let snapshot = state.lock().expect("state");
        assert_eq!(snapshot.invocations, ["call-1", "call-2"]);
        assert_eq!(snapshot.model_requests.len(), 2);
        assert!(matches!(
            snapshot.model_requests[1].messages.as_slice(),
            [
                ModelMessage::User { .. },
                ModelMessage::Assistant { .. },
                ModelMessage::Tool { call_id, .. },
                ModelMessage::Tool { call_id: second, .. }
            ] if call_id == "call-1" && second == "call-2"
        ));
    }

    let transcript = host
        .transcript(&id)
        .await
        .expect("transcript")
        .expect("present");
    for (index, event) in transcript.events().iter().enumerate() {
        assert_eq!(event.seq().get(), u64::try_from(index + 1).expect("seq"));
    }
    let dispatches = transcript
        .events()
        .iter()
        .filter(|event| {
            matches!(
                event.kind(),
                TranscriptEventKind::ToolDispatchStarted { .. }
            )
        })
        .count();
    let results = transcript
        .events()
        .iter()
        .filter(|event| {
            matches!(
                event.kind(),
                TranscriptEventKind::ToolResult {
                    outcome: ToolOutcome::Succeeded { .. },
                    ..
                }
            )
        })
        .count();
    assert_eq!((dispatches, results), (2, 2));
}

#[tokio::test]
async fn schema_failure_is_model_visible_without_dispatch() {
    let temp = tempdir().expect("tempdir");
    let oversized_instance = "x".repeat(60 * 1024);
    let state = FakeState::shared(vec![
        call_message(vec![(
            "bad-call",
            json!({"wrong":oversized_instance}).to_string(),
        )]),
        final_message("recovered"),
    ]);
    let host = AgentHost::open_with_factory(
        AgentWorkspace::new(temp.path().join("agent")),
        Box::new(FakeFactory::new(Arc::clone(&state))),
    )
    .await
    .expect("host");
    let id = SessionId::new("schema-failure").expect("id");
    let record = host
        .run(RunRequest::new(id, "try an invalid call").expect("request"))
        .await
        .expect("run");
    assert!(matches!(record.status(), RunStatus::Completed { .. }));
    let snapshot = state.lock().expect("state");
    assert!(snapshot.invocations.is_empty());
    assert!(matches!(
        snapshot.model_requests[1].messages.last(),
        Some(ModelMessage::Tool {
            result: WireToolResult::Error { code, message },
            ..
        }) if code == "invalid_arguments"
            && message == "arguments do not match the captured schema"
    ));
}

#[tokio::test]
async fn lossy_underflow_is_model_visible_without_dispatch() {
    let temp = tempdir().expect("tempdir");
    let state = FakeState::shared(vec![
        call_message(vec![("tiny-positive", r#"{"number":1e-999}"#.to_owned())]),
        final_message("done"),
    ]);
    state.lock().expect("state").tool_schema = Some(json!({
        "type": "object",
        "properties": {"number": {"type": "number", "maximum": 0}},
        "required": ["number"],
        "additionalProperties": false
    }));
    let host = AgentHost::open_with_factory(
        AgentWorkspace::new(temp.path().join("agent")),
        Box::new(FakeFactory::new(Arc::clone(&state))),
    )
    .await
    .expect("host");
    let record = host
        .run(
            RunRequest::new(
                SessionId::new("schema-number-dispatch").expect("id"),
                "validate and invoke",
            )
            .expect("request"),
        )
        .await
        .expect("run");
    assert!(matches!(record.status(), RunStatus::Completed { .. }));
    let snapshot = state.lock().expect("state");
    assert!(snapshot.invocation_arguments.is_empty());
    assert!(matches!(
        snapshot.model_requests[1].messages.last(),
        Some(ModelMessage::Tool {
            result: WireToolResult::Error { code, message },
            ..
        }) if code == "invalid_arguments"
            && message == "arguments contain a number that is not exactly representable for schema validation"
    ));
}

#[tokio::test]
async fn lossy_large_integer_is_model_visible_without_dispatch() {
    let temp = tempdir().expect("tempdir");
    let state = FakeState::shared(vec![
        call_message(vec![(
            "large-odd-integer",
            r#"{"number":9007199254740993}"#.to_owned(),
        )]),
        final_message("done"),
    ]);
    state.lock().expect("state").tool_schema = Some(json!({
        "type": "object",
        "properties": {"number": {"type": "integer", "multipleOf": 2}},
        "required": ["number"],
        "additionalProperties": false
    }));
    let host = AgentHost::open_with_factory(
        AgentWorkspace::new(temp.path().join("agent")),
        Box::new(FakeFactory::new(Arc::clone(&state))),
    )
    .await
    .expect("host");
    let record = host
        .run(
            RunRequest::new(
                SessionId::new("schema-large-integer-dispatch").expect("id"),
                "validate and invoke",
            )
            .expect("request"),
        )
        .await
        .expect("run");
    assert!(matches!(record.status(), RunStatus::Completed { .. }));
    assert!(state.lock().expect("state").invocation_arguments.is_empty());
}

#[tokio::test]
async fn numeric_normalization_cannot_expand_arguments_past_the_dispatch_bound() {
    let temp = tempdir().expect("tempdir");
    let raw_arguments = format!("[{}]", vec!["0"; 21_845].join(","));
    assert!(raw_arguments.chars().count() <= rsi_agent_protocol::MAX_CONTENT_CHARS);
    let state = FakeState::shared(vec![
        call_message(vec![("expanding-numbers", raw_arguments)]),
        final_message("recovered"),
    ]);
    state.lock().expect("state").tool_schema = Some(json!({
        "type": "array",
        "items": {"type": "integer"}
    }));
    let host = AgentHost::open_with_factory(
        AgentWorkspace::new(temp.path().join("agent")),
        Box::new(FakeFactory::new(Arc::clone(&state))),
    )
    .await
    .expect("host");
    let record = host
        .run(
            RunRequest::new(
                SessionId::new("schema-expanded-arguments").expect("id"),
                "do not dispatch an oversized normalized value",
            )
            .expect("request"),
        )
        .await
        .expect("run");
    assert!(matches!(record.status(), RunStatus::Completed { .. }));
    let snapshot = state.lock().expect("state");
    assert!(snapshot.invocations.is_empty());
    assert!(matches!(
        snapshot.model_requests[1].messages.last(),
        Some(ModelMessage::Tool {
            result: WireToolResult::Error { code, .. },
            ..
        }) if code == "invalid_arguments"
    ));
}

#[tokio::test]
async fn duplicate_argument_keys_are_model_visible_without_dispatch() {
    let temp = tempdir().expect("tempdir");
    let state = FakeState::shared(vec![
        call_message(vec![(
            "duplicate-arguments",
            r#"{"text":"first","text":"second"}"#.to_owned(),
        )]),
        final_message("recovered"),
    ]);
    let host = AgentHost::open_with_factory(
        AgentWorkspace::new(temp.path().join("agent")),
        Box::new(FakeFactory::new(Arc::clone(&state))),
    )
    .await
    .expect("host");
    let record = host
        .run(
            RunRequest::new(
                SessionId::new("duplicate-arguments").expect("id"),
                "reject ambiguous arguments",
            )
            .expect("request"),
        )
        .await
        .expect("run");
    assert!(matches!(record.status(), RunStatus::Completed { .. }));
    let snapshot = state.lock().expect("state");
    assert!(snapshot.invocations.is_empty());
    assert!(matches!(
        snapshot.model_requests[1].messages.last(),
        Some(ModelMessage::Tool {
            result: WireToolResult::Error { code, message },
            ..
        }) if code == "invalid_arguments"
            && message == "arguments are not bounded JSON with unique object keys"
    ));
}

#[tokio::test]
async fn unknown_tool_is_model_visible_without_dispatch() {
    let temp = tempdir().expect("tempdir");
    let state = FakeState::shared(vec![
        language_output(
            None,
            vec![rsi_ai_protocol::ToolCall {
                id: "unknown-call".to_owned(),
                name: "absent".to_owned(),
                arguments: "{}".to_owned(),
            }],
        ),
        final_message("recovered"),
    ]);
    let host = AgentHost::open_with_factory(
        AgentWorkspace::new(temp.path().join("agent")),
        Box::new(FakeFactory::new(Arc::clone(&state))),
    )
    .await
    .expect("host");
    let record = host
        .run(
            RunRequest::new(
                SessionId::new("unknown-tool").expect("id"),
                "request an unknown tool",
            )
            .expect("request"),
        )
        .await
        .expect("run");
    assert!(matches!(record.status(), RunStatus::Completed { .. }));
    let snapshot = state.lock().expect("state");
    assert!(snapshot.invocations.is_empty());
    assert!(matches!(
        snapshot.model_requests[1].messages.last(),
        Some(ModelMessage::Tool {
            result: WireToolResult::Error { code, .. },
            ..
        }) if code == "unknown_tool"
    ));
}

#[tokio::test(start_paused = true)]
async fn model_and_tool_response_deadlines_close_durable_failures() {
    let model_temp = tempdir().expect("tempdir");
    let model_state = FakeState::shared(Vec::new());
    model_state.lock().expect("state").model_pending = true;
    let model_host = AgentHost::open_with_factory(
        AgentWorkspace::new(model_temp.path().join("agent")),
        Box::new(FakeFactory::new(model_state)),
    )
    .await
    .expect("host");
    let model_record = model_host
        .run(
            RunRequest::new(
                SessionId::new("model-timeout").expect("id"),
                "wait for the model",
            )
            .expect("request"),
        )
        .await
        .expect("durable failure");
    assert!(matches!(
        model_record.status(),
        RunStatus::Failed {
            failure: Failure {
                kind: FailureKind::TimedOut,
                ..
            }
        }
    ));

    let tool_temp = tempdir().expect("tempdir");
    let tool_state = FakeState::shared(vec![call_message(vec![(
        "slow-tool",
        json!({"text":"wait"}).to_string(),
    )])]);
    tool_state.lock().expect("state").tool_pending = true;
    let tool_host = AgentHost::open_with_factory(
        AgentWorkspace::new(tool_temp.path().join("agent")),
        Box::new(FakeFactory::new(tool_state)),
    )
    .await
    .expect("host");
    let tool_id = SessionId::new("tool-timeout").expect("id");
    let tool_record = tool_host
        .run(RunRequest::new(tool_id.clone(), "wait for the tool").expect("request"))
        .await
        .expect("durable failure");
    assert!(matches!(
        tool_record.status(),
        RunStatus::Failed {
            failure: Failure {
                kind: FailureKind::TimedOut,
                ..
            }
        }
    ));
    let transcript = tool_host
        .transcript(&tool_id)
        .await
        .expect("transcript")
        .expect("present");
    assert!(transcript.events().iter().any(|event| matches!(
        event.kind(),
        TranscriptEventKind::ToolResult {
            outcome: ToolOutcome::OutcomeUnknown,
            ..
        }
    )));
}

#[tokio::test]
async fn provider_tool_error_is_logged_and_returned_to_model() {
    let temp = tempdir().expect("tempdir");
    let state = FakeState::shared(vec![
        call_message(vec![("tool-error", json!({"text":"fail"}).to_string())]),
        final_message("handled"),
    ]);
    state.lock().expect("state").tool_error = true;
    let host = AgentHost::open_with_factory(
        AgentWorkspace::new(temp.path().join("agent")),
        Box::new(FakeFactory::new(Arc::clone(&state))),
    )
    .await
    .expect("host");
    let record = host
        .run(
            RunRequest::new(
                SessionId::new("tool-error").expect("id"),
                "handle a tool error",
            )
            .expect("request"),
        )
        .await
        .expect("run");
    assert!(matches!(record.status(), RunStatus::Completed { .. }));
    assert_eq!(host.commit_count(), 9, "one-tool completion commit budget");
    let snapshot = state.lock().expect("state");
    assert_eq!(snapshot.invocations, ["tool-error"]);
    assert!(matches!(
        snapshot.model_requests[1].messages.last(),
        Some(ModelMessage::Tool {
            result: WireToolResult::Error { code, .. },
            ..
        }) if code == "fixture_failure"
    ));
}

#[tokio::test]
async fn malformed_and_duplicate_model_outputs_close_failed() {
    for (session, script) in [
        (
            "empty-model-output",
            vec![language_output(None, Vec::new())],
        ),
        (
            "duplicate-call-id",
            vec![
                call_message(vec![("same", json!({"text":"one"}).to_string())]),
                call_message(vec![("same", json!({"text":"two"}).to_string())]),
            ],
        ),
        (
            "duplicate-call-id-same-response",
            vec![call_message(vec![
                ("same", json!({"text":"one"}).to_string()),
                ("same", json!({"text":"two"}).to_string()),
            ])],
        ),
        (
            "reasoning-only-final",
            vec![rsi_ai_protocol::LanguageOutput {
                content: vec![rsi_ai_protocol::ContentBlock::Reasoning {
                    text: "internal only".to_owned(),
                }],
                finish_reason: rsi_ai_protocol::FinishReason::Stop,
                usage: None,
                replay: None,
                warnings: Vec::new(),
                sources: Vec::new(),
            }],
        ),
    ] {
        let temp = tempdir().expect("tempdir");
        let state = FakeState::shared(script);
        let host = AgentHost::open_with_factory(
            AgentWorkspace::new(temp.path().join("agent")),
            Box::new(FakeFactory::new(state)),
        )
        .await
        .expect("host");
        let record = host
            .run(
                RunRequest::new(
                    SessionId::new(session).expect("id"),
                    "reject malformed model output",
                )
                .expect("request"),
            )
            .await
            .expect("durable failure");
        assert!(matches!(
            record.status(),
            RunStatus::Failed {
                failure: Failure {
                    kind: FailureKind::ModelProtocol,
                    ..
                }
            }
        ));
    }
}

#[tokio::test]
async fn oversized_assistant_event_closes_durably_without_poisoning_the_host() {
    let temp = tempdir().expect("tempdir");
    let state = FakeState::shared(vec![
        final_message(&"x".repeat(3 * 1024 * 1024)),
        final_message("still healthy"),
    ]);
    let host = AgentHost::open_with_factory(
        AgentWorkspace::new(temp.path().join("agent")),
        Box::new(FakeFactory::new(state)),
    )
    .await
    .expect("host");
    let first = host
        .run(
            RunRequest::new(
                SessionId::new("oversized-assistant-event").expect("id"),
                "reject before persistence",
            )
            .expect("request"),
        )
        .await
        .expect("durable bounded failure");
    assert!(matches!(
        first.status(),
        RunStatus::Failed {
            failure: Failure {
                kind: FailureKind::ContextLimitExceeded,
                ..
            }
        }
    ));
    let second = host
        .run(
            RunRequest::new(
                SessionId::new("post-oversized-assistant").expect("id"),
                "host remains usable",
            )
            .expect("request"),
        )
        .await
        .expect("second run");
    assert!(matches!(second.status(), RunStatus::Completed { .. }));
}

#[tokio::test]
async fn hostile_protocol_error_text_still_closes_a_durable_failure() {
    let temp = tempdir().expect("tempdir");
    let protocol_error = "unsupported protocol bad\0protocol".to_owned();
    assert!(protocol_error.contains('\0'));

    let state = FakeState::shared(Vec::new());
    state.lock().expect("state").model_error = Some(protocol_error);
    let host = AgentHost::open_with_factory(
        AgentWorkspace::new(temp.path().join("agent")),
        Box::new(FakeFactory::new(state)),
    )
    .await
    .expect("host");
    let id = SessionId::new("hostile-protocol-error").expect("id");
    let record = host
        .run(RunRequest::new(id.clone(), "fail durably").expect("request"))
        .await
        .expect("malformed provider data is a durable run failure");
    let RunStatus::Failed { failure } = record.status() else {
        panic!("expected failed status")
    };
    assert_eq!(failure.kind, FailureKind::ModelProtocol);
    assert!(!failure.message.is_empty());
    assert!(!failure.message.contains('\0'));
    assert!(host.transcript(&id).await.expect("transcript").is_some());
}

#[tokio::test]
async fn oversized_derived_context_closes_as_bounded_failure() {
    let temp = tempdir().expect("tempdir");
    let calls = (0..8)
        .map(|index| {
            (
                format!("large-{index}"),
                json!({"text":index.to_string()}).to_string(),
            )
        })
        .collect::<Vec<_>>();
    let state = FakeState::shared(vec![call_message_owned(calls)]);
    state.lock().expect("state").tool_result_bytes = 100 * 1024;
    let host = AgentHost::open_with_factory(
        AgentWorkspace::new(temp.path().join("agent")),
        Box::new(FakeFactory::new(Arc::clone(&state))),
    )
    .await
    .expect("host");
    let record = host
        .run(
            RunRequest::new(
                SessionId::new("context-limit").expect("id"),
                "grow the model context",
            )
            .expect("request"),
        )
        .await
        .expect("durable failure");
    assert!(matches!(
        record.status(),
        RunStatus::Failed {
            failure: Failure {
                kind: FailureKind::ContextLimitExceeded,
                ..
            }
        }
    ));
    let snapshot = state.lock().expect("state");
    assert_eq!(snapshot.model_requests.len(), 1);
    assert_eq!(snapshot.invocations.len(), 8);
}

#[tokio::test]
async fn per_step_call_budget_rejects_oversized_response_before_dispatch() {
    let temp = tempdir().expect("tempdir");
    let calls = (0..=crate::MAX_TOOL_CALLS_PER_STEP)
        .map(|index| {
            (
                format!("oversized-{index}"),
                json!({"text":index.to_string()}).to_string(),
            )
        })
        .collect::<Vec<_>>();
    let state = FakeState::shared(vec![call_message_owned(calls)]);
    let host = AgentHost::open_with_factory(
        AgentWorkspace::new(temp.path().join("agent")),
        Box::new(FakeFactory::new(Arc::clone(&state))),
    )
    .await
    .expect("host");
    let record = host
        .run(
            RunRequest::new(
                SessionId::new("step-call-limit").expect("id"),
                "request too many calls in one step",
            )
            .expect("request"),
        )
        .await
        .expect("durable failure");
    assert!(matches!(
        record.status(),
        RunStatus::Failed {
            failure: Failure {
                kind: FailureKind::ModelProtocol,
                ..
            }
        }
    ));
    let snapshot = state.lock().expect("state");
    assert_eq!(snapshot.model_requests.len(), 1);
    assert!(snapshot.invocations.is_empty());
}

#[tokio::test]
async fn turn_call_budget_stops_before_seventeenth_dispatch() {
    let temp = tempdir().expect("tempdir");
    let first = (0..crate::MAX_TOOL_CALLS_PER_STEP)
        .map(|index| {
            (
                format!("first-{index}"),
                json!({"text":index.to_string()}).to_string(),
            )
        })
        .collect::<Vec<_>>();
    let second = (0..crate::MAX_TOOL_CALLS_PER_STEP)
        .map(|index| {
            (
                format!("second-{index}"),
                json!({"text":index.to_string()}).to_string(),
            )
        })
        .collect::<Vec<_>>();
    let state = FakeState::shared(vec![
        call_message_owned(first),
        call_message_owned(second),
        call_message(vec![(
            "seventeenth",
            json!({"text":"must not run"}).to_string(),
        )]),
    ]);
    let host = AgentHost::open_with_factory(
        AgentWorkspace::new(temp.path().join("agent")),
        Box::new(FakeFactory::new(Arc::clone(&state))),
    )
    .await
    .expect("host");
    let record = host
        .run(
            RunRequest::new(
                SessionId::new("turn-call-limit").expect("id"),
                "exhaust the turn call budget",
            )
            .expect("request"),
        )
        .await
        .expect("durable failure");
    assert!(matches!(
        record.status(),
        RunStatus::Failed {
            failure: Failure {
                kind: FailureKind::CallLimitExceeded,
                ..
            }
        }
    ));
    let snapshot = state.lock().expect("state");
    assert_eq!(snapshot.model_requests.len(), 3);
    assert_eq!(snapshot.invocations.len(), crate::MAX_TOOL_CALLS_PER_TURN);
    assert!(!snapshot.invocations.iter().any(|id| id == "seventeenth"));
}

#[tokio::test]
async fn final_step_tool_call_is_not_dispatched() {
    let temp = tempdir().expect("tempdir");
    let scripts = (1..=crate::MAX_STEPS)
        .map(|step| {
            call_message(vec![(
                &format!("call-{step}"),
                json!({"text":step.to_string()}).to_string(),
            )])
        })
        .collect();
    let state = FakeState::shared(scripts);
    let host = AgentHost::open_with_factory(
        AgentWorkspace::new(temp.path().join("agent")),
        Box::new(FakeFactory::new(Arc::clone(&state))),
    )
    .await
    .expect("host");
    let id = SessionId::new("step-limit").expect("id");
    let record = host
        .run(RunRequest::new(id.clone(), "keep calling echo").expect("request"))
        .await
        .expect("run");
    assert!(matches!(
        record.status(),
        RunStatus::Failed {
            failure: Failure {
                kind: FailureKind::StepLimitExceeded,
                ..
            }
        }
    ));
    assert_eq!(
        state.lock().expect("state").invocations.len(),
        usize::try_from(crate::MAX_STEPS - 1).expect("steps")
    );
    let transcript = host
        .transcript(&id)
        .await
        .expect("transcript")
        .expect("session");
    assert!(transcript.events().iter().any(|event| matches!(
        event.kind(),
        TranscriptEventKind::ToolResult {
            call_id,
            outcome: ToolOutcome::NotStarted { reason },
        } if call_id.as_str() == "call-8" && reason == "step_limit_exceeded"
    )));
}

#[test]
fn durable_identifiers_and_prompt_controls_cannot_bypass_admission() {
    for encoded in [r#"""#, r#""contains space""#, r#""unicode-界""#] {
        assert!(serde_json::from_str::<SessionId>(encoded).is_err());
        assert!(serde_json::from_str::<crate::CallId>(encoded).is_err());
    }
    let id = SessionId::new("validated-id").expect("id");
    for prompt in ["nul\0prompt", "del\u{007f}prompt"] {
        assert!(matches!(
            RunRequest::new(id.clone(), prompt),
            Err(AgentError::InvalidInput {
                field: "prompt",
                ..
            })
        ));
    }
}

fn final_message(content: &str) -> rsi_ai_protocol::LanguageOutput {
    language_output(Some(content.to_owned()), Vec::new())
}

fn call_message(calls: Vec<(&str, String)>) -> rsi_ai_protocol::LanguageOutput {
    language_output(
        None,
        calls
            .into_iter()
            .map(|(id, arguments)| rsi_ai_protocol::ToolCall {
                id: id.to_owned(),
                name: "echo".to_owned(),
                arguments,
            })
            .collect(),
    )
}

fn call_message_owned(calls: Vec<(String, String)>) -> rsi_ai_protocol::LanguageOutput {
    language_output(
        None,
        calls
            .into_iter()
            .map(|(id, arguments)| rsi_ai_protocol::ToolCall {
                id,
                name: "echo".to_owned(),
                arguments,
            })
            .collect(),
    )
}

fn language_output(
    content: Option<String>,
    tool_calls: Vec<rsi_ai_protocol::ToolCall>,
) -> rsi_ai_protocol::LanguageOutput {
    let requested_tools = !tool_calls.is_empty();
    let mut blocks = content
        .into_iter()
        .map(|text| rsi_ai_protocol::ContentBlock::Text { text })
        .collect::<Vec<_>>();
    blocks.extend(
        tool_calls
            .into_iter()
            .map(rsi_ai_protocol::ContentBlock::ToolCall),
    );
    rsi_ai_protocol::LanguageOutput {
        content: blocks,
        finish_reason: if requested_tools {
            rsi_ai_protocol::FinishReason::ToolCalls
        } else {
            rsi_ai_protocol::FinishReason::Stop
        },
        usage: None,
        replay: None,
        warnings: Vec::new(),
        sources: Vec::new(),
    }
}

#[derive(Debug)]
struct FakeState {
    scripts: VecDeque<rsi_ai_protocol::LanguageOutput>,
    model_requests: Vec<ModelCompleteRequest>,
    invocations: Vec<String>,
    invocation_arguments: Vec<String>,
    opens: usize,
    tool_error: bool,
    tool_result_bytes: usize,
    tool_schema: Option<serde_json::Value>,
    model_gate: Option<Arc<ModelGate>>,
    model_error: Option<String>,
    model_failures: VecDeque<rsi_ai_protocol::AiError>,
    model_pending: bool,
    tool_pending: bool,
}

impl FakeState {
    fn shared(scripts: Vec<rsi_ai_protocol::LanguageOutput>) -> Arc<Mutex<Self>> {
        Arc::new(Mutex::new(Self {
            scripts: scripts.into(),
            model_requests: Vec::new(),
            invocations: Vec::new(),
            invocation_arguments: Vec::new(),
            opens: 0,
            tool_error: false,
            tool_result_bytes: 0,
            tool_schema: None,
            model_gate: None,
            model_error: None,
            model_failures: VecDeque::new(),
            model_pending: false,
            tool_pending: false,
        }))
    }
}

#[derive(Debug)]
struct ModelGate {
    entered: Semaphore,
    released: Semaphore,
}

impl Default for ModelGate {
    fn default() -> Self {
        Self {
            entered: Semaphore::new(0),
            released: Semaphore::new(0),
        }
    }
}

impl ModelGate {
    async fn wait_until_entered(&self) {
        self.entered.acquire().await.expect("model gate").forget();
    }

    fn release(&self, count: usize) {
        self.released.add_permits(count);
    }
}

struct FakeFactory {
    state: Arc<Mutex<FakeState>>,
}

impl FakeFactory {
    fn new(state: Arc<Mutex<FakeState>>) -> Self {
        Self { state }
    }
}

impl PortFactory for FakeFactory {
    fn open(&self, _session_id: &SessionId) -> std::result::Result<PortBundle, PortError> {
        self.state.lock().expect("state").opens += 1;
        Ok(PortBundle {
            model: Box::new(FakeModelPort {
                state: Arc::clone(&self.state),
            }),
            tools: Box::new(FakeToolPort {
                state: Arc::clone(&self.state),
            }),
        })
    }
}

struct FakeModelPort {
    state: Arc<Mutex<FakeState>>,
}

fn fake_prepared_snapshot(request: &CommittedModelRequest) -> rsi_ai_meta::PreparedCallSnapshot {
    rsi_ai_meta::PreparedCallSnapshot {
        call_id: request.request_id().to_owned(),
        deployment_id: "fake-model".to_owned(),
        provider_family: "fake".to_owned(),
        capability: rsi_ai_meta::Capability::Language,
        model: request.model().to_owned(),
        protocol: "fixture".to_owned(),
        transport: "memory".to_owned(),
        endpoint_fingerprint: "fixture".to_owned(),
        config_generation: 1,
        credential_source: None,
        retry_policy: rsi_ai_meta::RetryPolicy::default(),
        request_sha256: crate::digest::sha256_hex(request.bytes()),
    }
}

fn legacy_request(bytes: &[u8]) -> std::result::Result<ModelCompleteRequest, PortError> {
    use rsi_ai_protocol::{MessageContent, MessageRole};

    let request = serde_json::from_slice::<rsi_ai_protocol::LanguageRequest>(bytes)
        .map_err(|error| protocol_error(error.to_string()))?;
    let mut system_prompt = None;
    let mut messages = Vec::new();
    for message in request.messages() {
        let text = message
            .content()
            .iter()
            .filter_map(|block| match block {
                MessageContent::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .collect::<String>();
        match message.role() {
            MessageRole::System | MessageRole::Developer => {
                system_prompt = Some(text);
            }
            MessageRole::User => messages.push(ModelMessage::User { content: text }),
            MessageRole::Assistant => {
                let tool_calls = message
                    .content()
                    .iter()
                    .filter_map(|block| match block {
                        MessageContent::ToolCall(call) => Some(LegacyToolCall {
                            id: call.id.clone(),
                            name: call.name.clone(),
                            arguments: call.arguments.clone(),
                        }),
                        _ => None,
                    })
                    .collect();
                messages.push(ModelMessage::Assistant {
                    content: (!text.is_empty()).then_some(text),
                    tool_calls,
                });
            }
            MessageRole::Tool => {
                let MessageContent::ToolResult {
                    call_id, content, ..
                } = &message.content()[0]
                else {
                    return Err(protocol_error("tool result shape mismatch"));
                };
                let raw = content
                    .iter()
                    .filter_map(|block| match block {
                        MessageContent::Text { text } => Some(text.as_str()),
                        _ => None,
                    })
                    .collect::<String>();
                let result = serde_json::from_str(&raw)
                    .map_err(|error| protocol_error(error.to_string()))?;
                messages.push(ModelMessage::Tool {
                    call_id: call_id.clone(),
                    result,
                });
            }
        }
    }
    let tools = request
        .tools()
        .iter()
        .map(|tool| rsi_agent_protocol::ToolDefinition {
            name: tool.name().to_owned(),
            description: tool.description().to_owned(),
            input_schema: tool.input_schema().clone(),
        })
        .collect();
    Ok(ModelCompleteRequest {
        system_prompt: system_prompt.ok_or_else(|| protocol_error("system prompt missing"))?,
        messages,
        tools,
    })
}

#[async_trait]
impl ModelPort for FakeModelPort {
    fn provider(&self) -> &'static str {
        "fake-model"
    }

    async fn initialize(&mut self) -> std::result::Result<(), PortError> {
        Ok(())
    }

    async fn prepare(
        &mut self,
        committed: &CommittedModelRequest,
    ) -> std::result::Result<PreparedModelCall, PortError> {
        let request = serde_json::from_slice::<rsi_ai_protocol::LanguageRequest>(committed.bytes())
            .map_err(|error| protocol_error(error.to_string()))?;
        request
            .validate()
            .map_err(|error| protocol_error(error.to_string()))?;
        Ok(PreparedModelCall::new(
            committed.clone(),
            fake_prepared_snapshot(committed),
        ))
    }

    async fn start(
        &mut self,
        prepared: PreparedModelCall,
    ) -> std::result::Result<ValidatedAssistantMessage, PortError> {
        let request = legacy_request(prepared.request().bytes())?;
        let (message, gate, error, failure, pending) = {
            let mut state = self.state.lock().expect("state");
            state.model_requests.push(request);
            let failure = state.model_failures.pop_front();
            let message = failure
                .is_none()
                .then(|| state.scripts.pop_front())
                .flatten();
            (
                message,
                state.model_gate.clone(),
                state.model_error.clone(),
                failure,
                state.model_pending,
            )
        };
        if let Some(error) = failure {
            return Err(PortError::model_ai(error, false));
        }
        if let Some(error) = error {
            return Err(protocol_error(error));
        }
        if pending {
            return std::future::pending().await;
        }
        if let Some(gate) = gate {
            gate.entered.add_permits(1);
            gate.released.acquire().await.expect("model gate").forget();
        }
        let message = message.ok_or_else(|| protocol_error("script exhausted"))?;
        ValidatedAssistantMessage::validate(message)
    }

    async fn finish(&mut self) -> std::result::Result<(), PortError> {
        Ok(())
    }
}

struct FakeToolPort {
    state: Arc<Mutex<FakeState>>,
}

#[async_trait]
impl ToolPort for FakeToolPort {
    fn provider(&self) -> &'static str {
        "fake-tools"
    }

    async fn initialize(&mut self) -> std::result::Result<(), PortError> {
        Ok(())
    }

    async fn catalog(&mut self) -> std::result::Result<ValidatedToolCatalog, PortError> {
        let input_schema = self
            .state
            .lock()
            .expect("state")
            .tool_schema
            .clone()
            .unwrap_or_else(|| {
                json!({
                    "$schema": "https://json-schema.org/draft/2020-12/schema",
                    "type": "object",
                    "properties": {"text": {"type": "string"}},
                    "required": ["text"],
                    "additionalProperties": false
                })
            });
        ValidatedToolCatalog::validate(ToolsCatalogResponse {
            tools: vec![rsi_agent_protocol::ToolDefinition {
                name: "echo".to_owned(),
                description: "Return the supplied text.".to_owned(),
                input_schema,
            }],
        })
    }

    async fn invoke(
        &mut self,
        request: ToolsInvokeRequest,
    ) -> std::result::Result<ValidatedToolResponse, PortError> {
        let expected_call_id = request.call_id.clone();
        let (tool_error, tool_result_bytes, pending) = {
            let mut state = self.state.lock().expect("state");
            state.invocations.push(request.call_id.clone());
            state.invocation_arguments.push(request.arguments.clone());
            (
                state.tool_error,
                state.tool_result_bytes,
                state.tool_pending,
            )
        };
        if pending {
            return std::future::pending().await;
        }
        if tool_error {
            return ValidatedToolResponse::validate(
                ToolsInvokeResponse {
                    call_id: request.call_id,
                    result: WireToolResult::Error {
                        code: "fixture_failure".to_owned(),
                        message: "the fixture rejected the call".to_owned(),
                    },
                },
                &expected_call_id,
            );
        }
        let value: serde_json::Value =
            serde_json::from_str(&request.arguments).map_err(protocol_error)?;
        let value = if tool_result_bytes == 0 {
            value
        } else {
            json!({"blob":"x".repeat(tool_result_bytes)})
        };
        ValidatedToolResponse::validate(
            ToolsInvokeResponse {
                call_id: request.call_id,
                result: WireToolResult::Ok { value },
            },
            &expected_call_id,
        )
    }

    async fn finish(&mut self) -> std::result::Result<(), PortError> {
        Ok(())
    }
}

fn protocol_error(error: impl std::fmt::Display) -> PortError {
    PortError {
        failure: Failure::new(FailureKind::ModelProtocol, error.to_string()),
        retry: None,
    }
}

#[cfg(feature = "test-failpoints")]
#[test]
#[allow(clippy::too_many_lines)] // The table and identical recovery assertions belong together.
fn crash_boundaries_recover_without_reexecution() {
    use std::process::Command;

    for (stage, expected_tool_outcome, expected_status) in [
        ("after_session_created", None, RunStatus::Interrupted),
        ("after_model_request", None, RunStatus::Interrupted),
        (
            "after_tool_prepared",
            Some("not_started"),
            RunStatus::Interrupted,
        ),
        (
            "after_tool_dispatch",
            Some("outcome_unknown"),
            RunStatus::Interrupted,
        ),
        (
            "after_second_tool_dispatch",
            Some("outcome_unknown"),
            RunStatus::Interrupted,
        ),
        ("after_followup_request", None, RunStatus::Interrupted),
        (
            "after_call_limit_prepared",
            Some("not_started"),
            RunStatus::Interrupted,
        ),
        ("after_final_assistant", None, RunStatus::Interrupted),
        (
            "after_terminal_commit",
            None,
            RunStatus::Completed {
                final_message: "durably completed".to_owned(),
            },
        ),
    ] {
        let temp = tempdir().expect("tempdir");
        let workspace = temp.path().join("agent");
        let output = Command::new(std::env::current_exe().expect("test executable"))
            .args(["--exact", "tests::crash_child_driver", "--nocapture"])
            .env("RSI_AGENT_CRASH_CHILD", "1")
            .env("RSI_AGENT_CRASH_AT", stage)
            .env("RSI_AGENT_CRASH_WORKSPACE", &workspace)
            .output()
            .expect("spawn crash child");
        assert_eq!(
            output.status.code(),
            Some(86),
            "stage {stage} did not crash:\n{}",
            String::from_utf8_lossy(&output.stderr)
        );

        let runtime = tokio::runtime::Runtime::new().expect("runtime");
        runtime.block_on(async {
            let state = FakeState::shared(Vec::new());
            let host = AgentHost::open_with_factory(
                AgentWorkspace::new(&workspace),
                Box::new(FakeFactory::new(Arc::clone(&state))),
            )
            .await
            .expect("recovered host");
            let id = SessionId::new("crash-session").expect("id");
            let transcript = host
                .transcript(&id)
                .await
                .expect("transcript")
                .expect("present");
            assert_eq!(transcript.status(), &expected_status);
            match expected_tool_outcome {
                Some("not_started") => assert!(transcript.events().iter().any(|event| matches!(
                    event.kind(),
                    TranscriptEventKind::ToolResult {
                        outcome: ToolOutcome::NotStarted { .. },
                        ..
                    }
                ))),
                Some("outcome_unknown") => {
                    assert!(transcript.events().iter().any(|event| matches!(
                        event.kind(),
                        TranscriptEventKind::ToolResult {
                            outcome: ToolOutcome::OutcomeUnknown,
                            ..
                        }
                    )));
                }
                None => {}
                Some(other) => panic!("unexpected test expectation {other}"),
            }
            if stage == "after_second_tool_dispatch" {
                assert!(transcript.events().iter().any(|event| matches!(
                    event.kind(),
                    TranscriptEventKind::ToolResult {
                        outcome: ToolOutcome::Succeeded { .. },
                        ..
                    }
                )));
            }
            let replay = host
                .run(RunRequest::new(id, "crash this run").expect("request"))
                .await
                .expect("interrupted replay");
            assert_eq!(replay.status(), &expected_status);
            let snapshot = state.lock().expect("state");
            assert_eq!(snapshot.opens, 0);
            assert!(snapshot.model_requests.is_empty());
            assert!(snapshot.invocations.is_empty());
        });
    }
}

#[cfg(feature = "test-failpoints")]
#[test]
fn crash_child_driver() {
    if std::env::var_os("RSI_AGENT_CRASH_CHILD").is_none() {
        return;
    }
    let stage = std::env::var("RSI_AGENT_CRASH_AT").expect("crash stage");
    let workspace = std::env::var_os("RSI_AGENT_CRASH_WORKSPACE").expect("workspace");
    let script = match stage.as_str() {
        "after_final_assistant" => vec![final_message("never durably completed")],
        "after_terminal_commit" => vec![final_message("durably completed")],
        "after_call_limit_prepared" => {
            let first = (0..crate::MAX_TOOL_CALLS_PER_STEP)
                .map(|index| {
                    (
                        format!("first-{index}"),
                        json!({"text":index.to_string()}).to_string(),
                    )
                })
                .collect();
            let second = (0..crate::MAX_TOOL_CALLS_PER_STEP)
                .map(|index| {
                    (
                        format!("second-{index}"),
                        json!({"text":index.to_string()}).to_string(),
                    )
                })
                .collect();
            vec![
                call_message_owned(first),
                call_message_owned(second),
                call_message(vec![(
                    "seventeenth",
                    json!({"text":"must not run"}).to_string(),
                )]),
            ]
        }
        "after_second_tool_dispatch" => vec![call_message(vec![
            ("first-call", json!({"text":"one"}).to_string()),
            ("second-call", json!({"text":"two"}).to_string()),
        ])],
        _ => vec![call_message(vec![(
            "crash-call",
            json!({"text":"hello"}).to_string(),
        )])],
    };
    let runtime = tokio::runtime::Runtime::new().expect("runtime");
    runtime.block_on(async {
        let fake_state = FakeState::shared(script);
        let host = AgentHost::open_with_factory(
            AgentWorkspace::new(workspace),
            Box::new(FakeFactory::new(fake_state)),
        )
        .await
        .expect("child host");
        let request = RunRequest::new(
            SessionId::new("crash-session").expect("id"),
            "crash this run",
        )
        .expect("request");
        let outcome = host.run(request).await;
        panic!("crash failpoint {stage} did not exit; outcome: {outcome:?}");
    });
}
