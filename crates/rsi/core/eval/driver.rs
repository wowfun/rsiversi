use futures_util::StreamExt;
use rsi::{
    HostProfileDocument, HostProfileId, ProfileSource, ServiceHostConnectionMode,
    StandardCodingTools, StandardComposition, StandardServiceDaemon,
};
use rsi_agent_goal::{GoalAction, GoalPhase, GoalState};
use rsi_agent_session_protocol::{DomainRequestId, SessionId, WorkspaceTrust};
use rsi_credentials_local::SecretStore;
use rsi_credentials_protocol::{CredentialsError, SecretValue};
use rsi_goal::{GoalControl, GoalDriverStage};
use rsi_service_host::{HostOwnerLease, ServiceHostPaths};
use rsi_session_protocol::{CreateSession, SessionHandle};
use serde::Deserialize;
use std::{io::Read as _, sync::Arc};
use tokio_util::sync::CancellationToken;

type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;

async fn infrastructure<T>(phase: &str, work: impl std::future::Future<Output = T>) -> Result<T> {
    tokio::time::timeout(std::time::Duration::from_secs(30), work)
        .await
        .map_err(|_| format!("infrastructure deadline exceeded: {phase}").into())
}

async fn shutdown_owners(
    stop: &CancellationToken,
    client: impl std::future::Future<Output = Result<()>>,
    daemon: impl std::future::Future<Output = Result<()>>,
    host: impl std::future::Future<Output = Result<()>>,
) -> Result<()> {
    let client = infrastructure("client shutdown", client).await;
    stop.cancel();
    let daemon = infrastructure("daemon shutdown", daemon).await;
    let host = infrastructure("Host shutdown", host).await;
    let errors = [("client", client), ("daemon", daemon), ("Host", host)]
        .into_iter()
        .filter_map(|(phase, result)| {
            result
                .and_then(std::convert::identity)
                .err()
                .map(|error| format!("{phase}: {error}"))
        })
        .collect::<Vec<_>>();
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors.join("; ").into())
    }
}

fn with_cleanup(
    outcome: Result<serde_json::Value>,
    cleanup: Result<()>,
) -> Result<serde_json::Value> {
    match (outcome, cleanup) {
        (Ok(mut report), Err(error)) => {
            report
                .as_object_mut()
                .ok_or("API outcome is not an object")?
                .entry("cleanup_errors")
                .or_insert_with(|| serde_json::json!([]))
                .as_array_mut()
                .ok_or("cleanup diagnostics are not an array")?
                .push(serde_json::json!(error.to_string()));
            Ok(report)
        }
        (Err(error), Err(cleanup)) => Err(format!("{error}; cleanup: {cleanup}").into()),
        (outcome, Ok(())) => outcome,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(start_paused = true)]
    async fn infrastructure_calls_expire_without_waiting_for_the_task_budget() {
        for phase in ["create", "attach", "control", "read", "close"] {
            let before = tokio::time::Instant::now();
            let error = infrastructure(phase, std::future::pending::<()>())
                .await
                .unwrap_err();
            assert!(error.to_string().contains(phase));
            assert_eq!(before.elapsed(), std::time::Duration::from_secs(30));
        }
        assert_eq!(infrastructure("read", async { 42 }).await.unwrap(), 42);
    }

    #[tokio::test(start_paused = true)]
    async fn cleanup_attempts_every_owner_after_failure_or_deadline() {
        for fault in 0..5 {
            let stop = CancellationToken::new();
            let phases = std::sync::Mutex::new(Vec::new());
            let result = shutdown_owners(
                &stop,
                async {
                    phases.lock().unwrap().push("client");
                    if fault == 0 {
                        return Err("client fault".into());
                    }
                    if fault == 1 {
                        std::future::pending::<()>().await;
                    }
                    Ok(())
                },
                async {
                    assert!(stop.is_cancelled());
                    phases.lock().unwrap().push("daemon");
                    if fault == 2 {
                        return Err("daemon fault".into());
                    }
                    if fault == 3 {
                        std::future::pending::<()>().await;
                    }
                    Ok(())
                },
                async {
                    phases.lock().unwrap().push("Host");
                    if fault == 4 {
                        return Err("Host fault".into());
                    }
                    Ok(())
                },
            )
            .await;
            assert!(result.is_err(), "fault {fault}");
            assert!(stop.is_cancelled(), "fault {fault}");
            assert_eq!(
                *phases.lock().unwrap(),
                ["client", "daemon", "Host"],
                "fault {fault}"
            );
        }
    }

    #[test]
    fn cleanup_errors_preserve_the_api_outcome_and_all_cleanup_diagnostics() {
        let report = serde_json::json!({"goal":{"phase":"completed"}});
        let failed = with_cleanup(Ok(report.clone()), Err("API cleanup fault".into())).unwrap();
        let failed = with_cleanup(Ok(failed), Err("Host cleanup fault".into())).unwrap();
        assert_eq!(failed["goal"], report["goal"]);
        assert_eq!(
            failed["cleanup_errors"],
            serde_json::json!(["API cleanup fault", "Host cleanup fault"])
        );
        assert_eq!(with_cleanup(Ok(report.clone()), Ok(())).unwrap(), report);
        assert!(
            with_cleanup(Err("execution failed".into()), Err("cleanup failed".into()))
                .unwrap_err()
                .to_string()
                .contains("execution failed; cleanup: cleanup failed")
        );
    }
}

#[derive(Debug)]
struct NoCredentialStore;
impl SecretStore for NoCredentialStore {
    fn get(&self, _: &str, _: &str) -> rsi_credentials_protocol::Result<Option<SecretValue>> {
        Ok(None)
    }
    fn set(&self, _: &str, _: &str, _: &SecretValue) -> rsi_credentials_protocol::Result<()> {
        Err(CredentialsError::Store(
            "evaluation has no credential store".into(),
        ))
    }
    fn unset(&self, _: &str, _: &str) -> rsi_credentials_protocol::Result<bool> {
        Err(CredentialsError::Store(
            "evaluation has no credential store".into(),
        ))
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Evaluation {
    session: SessionId,
    goal: DomainRequestId,
    request_id: DomainRequestId,
    create_session: bool,
    objective: String,
    constraints: String,
    max_rounds: u64,
}

pub(super) async fn main() -> std::process::ExitCode {
    match run().await {
        Ok(report) => {
            let complete = report["goal"]["goal"]["phase"] == "completed";
            println!("{report}");
            if report.get("cleanup_errors").is_some() {
                std::process::ExitCode::FAILURE
            } else if complete {
                std::process::ExitCode::SUCCESS
            } else {
                std::process::ExitCode::from(2)
            }
        }
        Err(error) => {
            eprintln!("Session API evaluation failed: {error}");
            std::process::ExitCode::FAILURE
        }
    }
}
async fn run() -> Result<serde_json::Value> {
    let mut bytes = Vec::new();
    std::io::stdin()
        .take(32 * 1024 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > 32 * 1024 {
        return Err("evaluation request exceeds 32 KiB".into());
    }
    let request: Evaluation = serde_json::from_slice(&bytes)?;
    if request.objective.len() > 8192 || request.constraints.len() > 4096 || request.max_rounds == 0
    {
        return Err("invalid bounded Goal request".into());
    }
    let paths = rsi::standard_paths()?;
    let profile_path = paths.config().join("host-profiles/live/host.profile.toml");
    let profile = HostProfileDocument {
        id: HostProfileId::new("live")?,
        source: ProfileSource::User,
        contents: std::fs::read(&profile_path)?,
        path: Some(profile_path),
    };
    let tools = StandardCodingTools::new(
        std::fs::canonicalize("/bin/bash")?,
        std::env::current_exe()?.canonicalize()?,
        rsi::scrub_child_environment(std::env::vars_os()),
    )?;
    let composition = StandardComposition::new(
        paths.clone(),
        rsi::capture_standard_environment()?,
        Some(tools),
    )
    .with_credential_store(Arc::new(NoCredentialStore));
    let host_paths = ServiceHostPaths::from_host_paths(&paths)?;
    host_paths.validate_daemon_endpoint()?;
    let owner = HostOwnerLease::try_acquire(host_paths)?;
    let daemon = infrastructure(
        "Host startup",
        StandardServiceDaemon::start(composition.clone(), &profile, owner),
    )
    .await??;
    let running = daemon.running();
    let stop = CancellationToken::new();
    let daemon_task = tokio::spawn(daemon.run(stop.clone()));
    let runtime = rsi_meta::Runtime::default();
    let outcome = async {
        let connection = infrastructure(
            "API connection",
            Box::pin(rsi::connect_or_embed_service_host(
                &runtime.root(),
                composition,
                &profile,
            )),
        )
        .await??;
        if connection.mode() != ServiceHostConnectionMode::Remote {
            return Err("evaluation did not connect through the local API".into());
        }
        let result = exercise(&connection, request).await;
        let closed = infrastructure("API shutdown", connection.shutdown())
            .await
            .and_then(|closed| closed.map_err(Into::into));
        with_cleanup(result, closed)
    }
    .await;
    let cleanup = shutdown_owners(
        &stop,
        async {
            if runtime.shutdown().await.is_clean() {
                Ok(())
            } else {
                Err("client cleanup failed".into())
            }
        },
        async {
            daemon_task.await??;
            Ok(())
        },
        async {
            if running.shutdown().await.is_clean() {
                Ok(())
            } else {
                Err("Host cleanup failed".into())
            }
        },
    )
    .await;
    with_cleanup(outcome, cleanup)
}

async fn state(handle: &Arc<dyn SessionHandle>) -> Result<GoalState> {
    let mut stream = infrastructure("projection admission", handle.observe_projections()).await??;
    let snapshot = infrastructure("projection read", stream.next())
        .await?
        .ok_or("Goal projection ended")??;
    let entry = snapshot
        .snapshot()
        .entries()
        .iter()
        .find(|entry| entry.producer().as_str() == rsi_agent_goal::GOAL_PROJECTION)
        .ok_or("Goal contribution absent")?;
    let state: GoalState = serde_json::from_value(
        entry
            .view()
            .ok_or("Goal projection failed")?
            .value()
            .clone(),
    )?;
    state.validate()?;
    Ok(state)
}

async fn exercise(
    connection: &rsi::ServiceHostConnection,
    request: Evaluation,
) -> Result<serde_json::Value> {
    let service = connection.session_service();
    let handle = if request.create_session {
        let workspace = infrastructure(
            "workspace registration",
            connection
                .workspace_registry()
                .get_or_create(&std::env::current_dir()?),
        )
        .await??;
        infrastructure(
            "Session create",
            service.create(CreateSession {
                session_id: request.session,
                workspace_id: workspace.id,
                agent_preset_id: None,
                workspace_trust: WorkspaceTrust::Untrusted,
            }),
        )
        .await??
    } else {
        infrastructure("Session attach", service.attach(&request.session)).await??
    };
    let before_live = infrastructure("Goal status", handle.goal_status()).await??;
    if before_live.armed {
        return Err("initial API read found an unexpectedly armed Goal".into());
    }
    let before = state(&handle).await?;
    let revision = infrastructure("command discovery", handle.commands())
        .await??
        .revision();
    let after_reads_live =
        infrastructure("Goal status after reads", handle.goal_status()).await??;
    if after_reads_live.armed {
        return Err("read-only API operations armed a Goal".into());
    }
    let receipt = infrastructure(
        "Goal control",
        handle.control_goal(GoalControl {
            request_id: request.request_id,
            expected_revision: revision,
            action: GoalAction::Create {
                id: request.goal,
                objective: request.objective,
                constraints: request.constraints,
                max_rounds: request.max_rounds,
            },
        }),
    )
    .await??;
    let mut stream = infrastructure("Goal observation admission", handle.observe_goal()).await??;
    let live = tokio::time::timeout(std::time::Duration::from_secs(450), async {
        loop {
            let value = stream.next().await.ok_or("live Goal stream ended")??;
            if matches!(
                value.stage,
                GoalDriverStage::Disarmed | GoalDriverStage::Failed
            ) {
                return Ok::<_, Box<dyn std::error::Error + Send + Sync>>(value);
            }
        }
    })
    .await??;
    let goal = state(&handle).await?;
    let current = goal.goal.as_ref().ok_or("Goal disappeared")?;
    if live.armed
        || current
            .reservation
            .as_ref()
            .is_none_or(|reservation| reservation.settlement.is_none())
    {
        return Err("driver stopped without canonical round settlement".into());
    }
    if current.phase == GoalPhase::Completed && current.report.is_none() {
        return Err("completion lacks an authenticated report".into());
    }
    Ok(
        serde_json::json!({"transport":"local_session_api", "before_live":before_live, "after_reads_live":after_reads_live, "before":before, "receipt":receipt, "live":live, "goal":goal}),
    )
}
