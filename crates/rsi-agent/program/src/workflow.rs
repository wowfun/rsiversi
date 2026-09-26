use crate::{ProgramRpc, ProgramRuntime, ProgramRuntimeContract};
use async_trait::async_trait;
use rsi_agent_session_protocol::{
    DomainIdentity, ForkTurnSelection, OutputContract, ProgramDomainGuard, ProgramOutcome,
};
use rsi_agent_turn_protocol::{
    AgentCallerAuthority, PrepareProgram, ProgramAgentRequest, ProgramRun, TurnService,
    TurnServiceContract,
};
use rsi_jobs::{JobScopeId, Jobs, JobsContract};
use rsi_meta::{ActivationPlan, MetaError};
use rsi_tools_protocol::{
    ToolDefinition, ToolError, ToolExecution, ToolExecutor, ToolLease, ToolRegistrarContract,
    ToolRegistration, ToolResult, ToolTimeoutPolicy,
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{sync::Arc, time::Duration};
use tokio_util::sync::CancellationToken;

pub(super) fn register(plan: &ActivationPlan) -> rsi_meta::Result<ToolLease> {
    let definition = ToolDefinition::new(
        "run_workflow",
        "Run a JavaScript workflow with workflow.agent({message, output_schema?}), workflow.pipeline, workflow.parallel, and awaited workflow.phase/log. Return curated JSON. Uses shell-equivalent Sandbox authority. Foreground observes for 30 seconds (maximum 60), then detaches; background detaches immediately. The background run has no total execution deadline. One run per initial human root Turn or finite Goal/Schedule round; children and completion Turns cannot start successors.",
        json!({
            "type": "object",
            "properties": {
                "script": {
                    "type": "string", "minLength": 1,
                    "maxLength": rsi_agent_session_protocol::MAXIMUM_PROGRAM_SCRIPT_BYTES,
                },
                "background": {"type": "boolean"},
                "observe_seconds": {"type": "integer", "minimum": 1, "maximum": 60},
                "fork_turns": {"type": "string"},
            },
            "required": ["script"],
            "additionalProperties": false,
        }),
    ).map_err(meta)?.with_program_role(rsi_tools_protocol::ToolProgramRole::Workflow);
    plan.local::<ToolRegistrarContract>()?
        .register(ToolRegistration {
            definition,
            output: None,
            timeout: ToolTimeoutPolicy::Execution {
                timeout_ms: 600_000,
            },
            executor: Arc::new(Workflow {
                runtime: plan.local::<ProgramRuntimeContract>()?,
                jobs: plan.local::<JobsContract>()?,
                turns: plan.local::<TurnServiceContract>()?,
            }),
        })
        .map_err(meta)
}
pub(super) fn meta(error: impl std::fmt::Display) -> MetaError {
    MetaError::Activation(error.to_string())
}
pub(super) fn failure(error: impl std::fmt::Display) -> ToolError {
    ToolError::Execution(error.to_string())
}
#[derive(Debug)]
struct Workflow {
    runtime: Arc<ProgramRuntime>,
    jobs: Arc<dyn Jobs>,
    turns: Arc<dyn TurnService>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Arguments {
    script: String,
    #[serde(default)]
    background: bool,
    #[serde(default = "observation")]
    observe_seconds: u64,
    #[serde(default)]
    fork_turns: Option<String>,
}
impl Arguments {
    fn validate(&self) -> rsi_tools_protocol::Result<()> {
        if self.script.is_empty()
            || self.script.len() > rsi_agent_session_protocol::MAXIMUM_PROGRAM_SCRIPT_BYTES
            || !(1..=60).contains(&self.observe_seconds)
        {
            return Err(ToolError::InvalidInput(
                "workflow script or observation interval is out of bounds".into(),
            ));
        }
        Ok(())
    }
}
fn observation() -> u64 {
    30
}
#[async_trait]
impl ToolExecutor for Workflow {
    async fn execute(
        &self,
        arguments: Value,
        execution: ToolExecution,
    ) -> rsi_tools_protocol::Result<ToolResult> {
        let args: Arguments = serde_json::from_value(arguments)
            .map_err(|e| ToolError::InvalidInput(e.to_string()))?;
        args.validate()?;
        let caller = execution
            .extension::<AgentCallerAuthority>()
            .ok_or_else(|| failure("workflow creator authority is absent"))?;
        let run = self.prepare_run(&args, &execution, &caller).await?;
        let scope = acquire_workflow_scope(
            self.jobs.as_ref(),
            caller.session_id(),
            &run.descriptor().run_id,
        )?;
        let mut program = match self
            .runtime
            .prepare_owned(
                args.script,
                &execution,
                &scope,
                Arc::new(WorkflowRpc(run.clone())),
                run.cancellation(),
            )
            .await
        {
            Ok(program) => program,
            Err(error) => {
                self.jobs.finalize_scope(&scope).await.map_err(failure)?;
                return Err(failure(error));
            }
        };
        if let Err(error) = run.accept(&caller).await {
            program.cancel();
            let _ = program.result().await;
            self.jobs.finalize_scope(&scope).await.map_err(failure)?;
            return Err(failure(error));
        }
        if let Err(error) = run.start().await {
            program.cancel();
            let _ = program.result().await;
            settle_setup_failure(
                run.finish(ProgramOutcome::Cancelled, None),
                self.jobs.finalize_scope(&scope),
            )
            .await?;
            return Err(failure(error));
        }
        let start = async {
            if args.background {
                run.detach().await.map_err(|e| e.to_string())?;
            }
            program.start()
        }
        .await;
        if let Err(error) = start {
            program.cancel();
            let _ = program.result().await;
            settle_setup_failure(
                run.finish(ProgramOutcome::Cancelled, None),
                self.jobs.finalize_scope(&scope),
            )
            .await?;
            return Err(failure(error));
        }
        let (finished, mut receiver) = tokio::sync::watch::channel(None);
        let owner = run.clone();
        let jobs = self.jobs.clone();
        let retiring = self.runtime.cancellation.clone();
        self.runtime.tasks.spawn(async move {
            finished.send_replace(Some(
                settle_workflow(owner, jobs, scope, program, retiring).await,
            ));
        });
        let identity =
            json!({"run_id":run.descriptor().run_id,"session_id":run.descriptor().session_id});
        if args.background {
            return ToolResult::new(
                json!({"run":identity,"status":"running","detached":true}),
                vec![],
                false,
            );
        }
        let observed=tokio::select! {biased;()=execution.cancellation.cancelled()=>{run.cancel_from_creator().await.map_err(failure)?;wait_finished(&mut receiver).await},result=wait_finished(&mut receiver)=>result,()=tokio::time::sleep(Duration::from_secs(args.observe_seconds))=>{
            if run.detach().await.is_ok() {return ToolResult::new(json!({"run":identity,"status":"running","detached":true}),vec![],false);}
            wait_finished(&mut receiver).await
        }}.map_err(failure)?;
        ToolResult::new(
            json!({"run":identity,"outcome":observed.0,"value":observed.1,"detached":false}),
            vec![],
            observed.0 != ProgramOutcome::Completed,
        )
    }
}
fn acquire_workflow_scope(
    jobs: &dyn Jobs,
    session: &rsi_agent_session_protocol::SessionId,
    run: &rsi_agent_session_protocol::ProgramRunId,
) -> rsi_tools_protocol::Result<rsi_jobs::JobScopeAuthority> {
    let id = JobScopeId::new(
        "rsi.agent.program.session",
        [session.to_string(), run.to_string()],
    )
    .map_err(failure)?;
    jobs.acquire_scope(id).map_err(failure)
}
impl Workflow {
    async fn prepare_run(
        &self,
        args: &Arguments,
        execution: &ToolExecution,
        caller: &AgentCallerAuthority,
    ) -> rsi_tools_protocol::Result<Arc<dyn ProgramRun>> {
        let domains = self
            .turns
            .domain_states(caller.session_id())
            .await
            .map_err(failure)?;
        let guard = workflow_guard(&domains)?;
        let run = self
            .turns
            .prepare_program(PrepareProgram {
                caller: caller.clone(),
                cancellation: execution.cancellation.clone(),
                script: args.script.clone(),
                fork_turns: args
                    .fork_turns
                    .as_deref()
                    .map_or(Ok(ForkTurnSelection::All), ForkTurnSelection::parse)
                    .map_err(failure)?,
                guard: Some(guard),
                continuation_domains: vec![
                    DomainIdentity::new(rsi_agent_goal::GOAL_DOMAIN, 1).map_err(failure)?,
                    DomainIdentity::new(rsi_agent_schedule::SCHEDULE_DOMAIN, 1).map_err(failure)?,
                ],
            })
            .await
            .map_err(failure)?;
        Ok(run)
    }
}
fn workflow_guard(
    domains: &[rsi_agent_session_protocol::DomainStateView],
) -> rsi_tools_protocol::Result<ProgramDomainGuard> {
    let state = domains
        .iter()
        .find(|state| state.snapshot.identity().id() == rsi_agent_plan_policy::PLAN_POLICY_DOMAIN)
        .ok_or_else(|| failure("workflow execution requires the plan-policy domain"))?;
    if state.snapshot.state().value() != &Value::Bool(false) {
        return Err(failure("workflow execution is unavailable in plan mode"));
    }
    Ok(ProgramDomainGuard {
        domain: state.snapshot.identity().clone(),
        revision: state.revision,
        snapshot_sha256: state.snapshot.sha256().map_err(failure)?,
    })
}
async fn settle_workflow(
    owner: Arc<dyn ProgramRun>,
    jobs: Arc<dyn Jobs>,
    scope: rsi_jobs::JobScopeAuthority,
    program: crate::AdmittedProgram,
    retiring: CancellationToken,
) -> Finished {
    let cancellation = owner.cancellation();
    let value = tokio::select! {
        biased;
        () = retiring.cancelled() => {
            let _ = owner.cancel().await;
            program.cancel();
            let _ = program.result().await;
            Err("workflow runtime retired".to_owned())
        }
        () = cancellation.cancelled() => {
            program.cancel();
            let _ = program.result().await;
            Err("workflow cancelled".to_owned())
        }
        value = program.result() => value,
    };
    let reported = jobs.wait(&scope, program.job_id(), 0, 0).await;
    let finalized = jobs.finalize_scope(&scope).await;
    let cleanup = reported.map(|_| ()).and(finalized.map(|_| ()));
    let outcome = match (&value, cleanup) {
        (_, _) if cancellation.is_cancelled() => ProgramOutcome::Cancelled,
        (_, Err(error)) => ProgramOutcome::Failed {
            code: "program.cleanup".into(),
            message: bounded(&error.to_string()),
        },
        (Ok(_), Ok(())) => ProgramOutcome::Completed,
        (Err(error), Ok(())) => ProgramOutcome::Failed {
            code: "program.execution".into(),
            message: bounded(error),
        },
    };
    let outcome = owner
        .finish(outcome, value.as_ref().ok().cloned())
        .await
        .map_err(|error| error.to_string())?;
    let value = value.ok().filter(|_| outcome == ProgramOutcome::Completed);
    Ok((outcome, value))
}
type Finished = Result<(ProgramOutcome, Option<Value>), String>;
async fn wait_finished(receiver: &mut tokio::sync::watch::Receiver<Option<Finished>>) -> Finished {
    loop {
        if let Some(value) = receiver.borrow().clone() {
            return value;
        }
        receiver
            .changed()
            .await
            .map_err(|_| "workflow owner closed".to_owned())?;
    }
}
fn bounded(value: &str) -> String {
    let mut result = String::new();
    for ch in value.chars().filter(|ch| *ch != '\0' && *ch != '\u{7f}') {
        if result.len() + ch.len_utf8() > 4096 {
            break;
        }
        result.push(ch);
    }
    if result.is_empty() {
        "Workflow execution failed.".into()
    } else {
        result
    }
}
#[derive(Debug)]
struct WorkflowRpc(Arc<dyn ProgramRun>);
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AgentArguments {
    message: String,
    #[serde(default)]
    output_schema: Option<Value>,
}
#[async_trait]
impl ProgramRpc for WorkflowRpc {
    fn definitions(&self) -> Value {
        json!([])
    }
    async fn call(
        &self,
        method: String,
        args: Value,
        cancellation: CancellationToken,
    ) -> Result<Value, String> {
        match method.as_str() {
            "agent" => {
                let args: AgentArguments =
                    serde_json::from_value(args).map_err(|e| e.to_string())?;
                let request = ProgramAgentRequest {
                    message: args.message,
                    output_contract: args
                        .output_schema
                        .map(OutputContract::from_model_schema)
                        .transpose()
                        .map_err(|e| e.to_string())?,
                    role: None,
                };
                let result = tokio::select! {biased;()=cancellation.cancelled()=>{self.0.cancel().await.map_err(|e|e.to_string())?;return Err("workflow RPC cancelled".into());},result=self.0.agent(request)=>result.map_err(|e|e.to_string())?};
                Ok(
                    json!({"receipt":result.receipt,"value":result.structured.as_ref().map(|result|result.value.clone()).or(result.reply.map(Value::String)),"reference":result.structured.map(|result|result.reference)}),
                )
            }
            "phase" | "log" => {
                let phase = if method == "phase" {
                    Some(
                        args.get("name")
                            .and_then(Value::as_str)
                            .ok_or_else(|| "phase requires a name".to_owned())?
                            .to_owned(),
                    )
                } else {
                    None
                };
                let value = args.get("value").cloned().unwrap_or(Value::Null);
                let message = if let Value::String(text) = value {
                    text
                } else {
                    serde_json::to_string(&value).map_err(|e| e.to_string())?
                };
                self.0
                    .progress(phase, message)
                    .await
                    .map_err(|e| e.to_string())?;
                Ok(Value::Null)
            }
            _ => Err("workflow admits only agent, phase and log RPCs".into()),
        }
    }
}

// Retire Jobs before publishing terminal state; attempt both even if either fails.
async fn settle_setup_failure(
    settle: impl std::future::Future<Output = rsi_agent_turn_protocol::Result<ProgramOutcome>>,
    finalize: impl std::future::Future<Output = rsi_jobs::Result<rsi_jobs::JobFinalization>>,
) -> rsi_tools_protocol::Result<()> {
    let finalized = finalize.await;
    let settled = settle.await;
    settled.map_err(failure)?;
    finalized.map(|_| ()).map_err(failure)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn workflow_requires_disabled_plan_policy_and_captures_its_revision() {
        use rsi_agent_session_protocol::{
            DomainRevision, DomainSnapshot, DomainStateValue, DomainStateView,
        };
        assert!(
            workflow_guard(&[])
                .unwrap_err()
                .to_string()
                .contains("requires the plan-policy")
        );
        for enabled in [false, true] {
            let state = DomainStateView {
                revision: DomainRevision::new(7),
                snapshot: DomainSnapshot::new(
                    DomainIdentity::new(rsi_agent_plan_policy::PLAN_POLICY_DOMAIN, 1).unwrap(),
                    DomainStateValue::new(Value::Bool(enabled)).unwrap(),
                ),
            };
            let result = workflow_guard(std::slice::from_ref(&state));
            if enabled {
                assert!(result.is_err());
            } else {
                let guard = result.unwrap();
                assert_eq!(guard.revision, state.revision);
                assert_eq!(guard.snapshot_sha256, state.snapshot.sha256().unwrap());
            }
        }
    }
    #[tokio::test]
    async fn predecessor_cleanup_cannot_revoke_a_successor_in_the_same_session() {
        let runtime = rsi_meta::Runtime::default();
        let jobs_plugin = runtime
            .root()
            .apply(
                rsi_meta::ResolvedFactory::linked(
                    "jobs",
                    "test",
                    rsi_meta::UpdateMode::Replayable,
                    Arc::new(rsi_jobs_local::JobsLocalFactory),
                ),
                Value::Null,
            )
            .await
            .unwrap();
        let jobs = runtime.root().lookup_local::<JobsContract>().unwrap();
        let session = rsi_agent_session_protocol::SessionId::new("same-session").unwrap();
        let previous = acquire_workflow_scope(
            jobs.as_ref(),
            &session,
            &rsi_agent_session_protocol::ProgramRunId::new("old").unwrap(),
        )
        .unwrap();
        let successor = acquire_workflow_scope(
            jobs.as_ref(),
            &session,
            &rsi_agent_session_protocol::ProgramRunId::new("new").unwrap(),
        )
        .unwrap();
        jobs.finalize_scope(&previous).await.unwrap();
        assert!(jobs.list(&previous).is_err());
        assert!(
            jobs.list(&successor).is_ok(),
            "predecessor must not revoke the successor authority"
        );
        jobs.finalize_scope(&successor).await.unwrap();
        assert!(jobs_plugin.dispose().await.is_clean());
        assert!(runtime.shutdown().await.is_clean());
    }

    #[tokio::test]
    async fn setup_terminal_failure_still_finalizes_owned_jobs_scope() {
        let finalized = std::sync::atomic::AtomicBool::new(false);
        let result = settle_setup_failure(
            async {
                Err(rsi_agent_turn_protocol::TurnError::Invalid(
                    "terminal failed".into(),
                ))
            },
            async {
                finalized.store(true, std::sync::atomic::Ordering::SeqCst);
                Ok(rsi_jobs::JobFinalization { unreported: vec![] })
            },
        )
        .await;
        assert!(finalized.load(std::sync::atomic::Ordering::SeqCst));
        assert!(result.unwrap_err().to_string().contains("terminal failed"));
    }
    #[tokio::test]
    async fn setup_cleanup_precedes_terminal_even_when_both_fail() {
        let finalized = std::sync::atomic::AtomicBool::new(false);
        let result = settle_setup_failure(
            async {
                assert!(finalized.load(std::sync::atomic::Ordering::SeqCst));
                Err(rsi_agent_turn_protocol::TurnError::Invalid(
                    "terminal failed".into(),
                ))
            },
            async {
                finalized.store(true, std::sync::atomic::Ordering::SeqCst);
                Err(rsi_jobs::JobsError::InvalidInput("cleanup failed".into()))
            },
        )
        .await;
        assert!(result.unwrap_err().to_string().contains("terminal failed"));
    }
    #[derive(Debug)]
    struct RecordingRun {
        jobs: Arc<dyn Jobs>,
        scope: rsi_jobs::JobScopeAuthority,
        durable: std::sync::Mutex<Option<ProgramOutcome>>,
    }
    #[async_trait]
    impl ProgramRun for RecordingRun {
        fn descriptor(&self) -> &rsi_agent_session_protocol::ProgramRunDescriptor {
            unreachable!()
        }
        fn cancellation(&self) -> CancellationToken {
            CancellationToken::new()
        }
        async fn accept(&self, _: &AgentCallerAuthority) -> rsi_agent_turn_protocol::Result<()> {
            unreachable!()
        }
        async fn start(&self) -> rsi_agent_turn_protocol::Result<()> {
            unreachable!()
        }
        async fn detach(&self) -> rsi_agent_turn_protocol::Result<()> {
            unreachable!()
        }
        async fn cancel_from_creator(&self) -> rsi_agent_turn_protocol::Result<bool> {
            unreachable!()
        }
        async fn cancel(&self) -> rsi_agent_turn_protocol::Result<()> {
            unreachable!()
        }
        async fn agent(
            &self,
            _: ProgramAgentRequest,
        ) -> rsi_agent_turn_protocol::Result<rsi_agent_turn_protocol::ProgramAgentResult> {
            unreachable!()
        }
        async fn progress(
            &self,
            _: Option<String>,
            _: String,
        ) -> rsi_agent_turn_protocol::Result<()> {
            unreachable!()
        }
        async fn finish(
            &self,
            outcome: ProgramOutcome,
            _: Option<Value>,
        ) -> rsi_agent_turn_protocol::Result<ProgramOutcome> {
            assert!(
                self.jobs.list(&self.scope).is_err(),
                "scope must already be finalized before terminal publication"
            );
            *self.durable.lock().unwrap() = Some(outcome.clone());
            Ok(outcome)
        }
    }

    #[tokio::test]
    async fn cleanup_failure_is_terminalized_before_foreground_reports_the_same_outcome() {
        use rsi_sandbox::{
            ConfinedProcess, EnforcementStamp, ProcessStdio, SandboxBackend, SandboxFileSystem,
            SandboxMode, SandboxNetwork, SandboxScratch,
        };
        let runtime = rsi_meta::Runtime::default();
        let plugin = runtime
            .root()
            .apply(
                rsi_meta::ResolvedFactory::linked(
                    "jobs",
                    "test",
                    rsi_meta::UpdateMode::Replayable,
                    Arc::new(rsi_jobs_local::JobsLocalFactory),
                ),
                Value::Null,
            )
            .await
            .unwrap();
        let jobs = runtime.root().lookup_local::<JobsContract>().unwrap();
        let scope = jobs
            .acquire_scope(JobScopeId::new("test", ["cleanup"]).unwrap())
            .unwrap();
        let owner = Arc::new(RecordingRun {
            jobs: jobs.clone(),
            scope: scope.clone(),
            durable: std::sync::Mutex::new(None),
        });
        let cwd = tempfile::tempdir().unwrap();
        let (outcome, result) =
            tokio::sync::watch::channel(Some(Ok(json!({"script":"succeeded"}))));
        // The process has already settled. Only the Jobs reporting path is faulted
        // by a missing job; no process is spawned or provider contacted.
        let request = Arc::new(crate::runtime::Request {
            spec: rsi_process::DuplexProcessSpec {
                process: ConfinedProcess {
                    owner: None,
                    stdio: ProcessStdio::Pipes,
                    program: std::env::current_exe().unwrap(),
                    arguments: vec![],
                    cwd: cwd.path().into(),
                    stamp: EnforcementStamp {
                        requested: SandboxMode::DangerFullAccess,
                        backend: SandboxBackend::Unconfined,
                        workspace: cwd.path().into(),
                        filesystem: SandboxFileSystem::Unconfined,
                        scratch: SandboxScratch::Host,
                        network: SandboxNetwork::Host,
                    },
                },
                environment: vec![],
                stdout_buffer_bytes: 1024,
                stderr_max_bytes: 1024,
                termination_grace_ms: 1,
            },
            script: String::new(),
            rpc: Arc::new(WorkflowRpc(owner.clone())),
            start: CancellationToken::new(),
            cancel: CancellationToken::new(),
            cancelled_at_settlement: std::sync::atomic::AtomicBool::new(false),
            outcome,
        });
        let program = crate::AdmittedProgram {
            id: "missing-job".into(),
            request,
            result,
            started: true,
        };
        let (reported, value) = settle_workflow(
            owner.clone(),
            jobs,
            scope,
            program,
            CancellationToken::new(),
        )
        .await
        .unwrap();
        assert!(
            matches!(&reported, ProgramOutcome::Failed { code, .. } if code == "program.cleanup")
        );
        assert_eq!(*owner.durable.lock().unwrap(), Some(reported));
        assert_eq!(
            value, None,
            "failed cleanup must not expose a successful script value"
        );
        assert!(plugin.dispose().await.is_clean());
        assert!(runtime.shutdown().await.is_clean());
    }
}
