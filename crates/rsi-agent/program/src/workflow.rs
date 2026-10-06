use crate::{ProgramError, ProgramRpc, ProgramRuntime, ProgramRuntimeContract};
mod owner;
use async_trait::async_trait;
use rsi_agent_session_protocol::{
    DomainIdentity, ForkTurnSelection, OutputContract, ProgramDomainGuard, ProgramOutcome,
    RUN_WORKFLOW_TOOL_NAME, WorkflowInvocationResult, WorkflowRunLocator,
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
        RUN_WORKFLOW_TOOL_NAME,
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
pub(super) fn turn_failure(error: rsi_agent_turn_protocol::TurnError) -> ToolError {
    ProgramError::from(error).into()
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
    async fn prepare(
        &self,
        arguments: &Value,
        execution: &ToolExecution,
    ) -> rsi_tools_protocol::Result<Option<rsi_tools_protocol::ToolProcess>> {
        let arguments: Arguments = serde_json::from_value(arguments.clone())
            .map_err(|error| ToolError::InvalidInput(error.to_string()))?;
        arguments.validate()?;
        self.runtime
            .prepare_process(execution)
            .await
            .map(Some)
            .map_err(ToolError::from)
    }
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
        let identity = WorkflowRunLocator {
            run_id: run.descriptor().run_id.clone(),
            session_id: run.descriptor().session_id.clone(),
        };
        let process = execution.take_prepared_process()?;
        let scope =
            acquire_workflow_scope(self.jobs.as_ref(), caller.session_id(), &identity.run_id)?;
        let background = args.background;
        let observation = Duration::from_secs(args.observe_seconds);
        let (ready, mut receiver, commands) = owner::spawn(
            self.runtime.clone(),
            self.jobs.clone(),
            run,
            scope,
            args,
            execution.clone(),
            caller,
            process,
        );
        ready
            .await
            .map_err(|_| ToolError::OutcomeUnknown)?
            .map_err(ToolError::from)?;
        if background {
            return invocation_result(WorkflowInvocationResult::running(identity), false);
        }
        let Some(observed) = observe_foreground(
            &execution.cancellation,
            observation,
            &mut receiver,
            &commands,
        )
        .await
        .map_err(ToolError::from)?
        else {
            return invocation_result(WorkflowInvocationResult::running(identity), false);
        };
        if observed.0 == ProgramOutcome::Interrupted {
            return Err(ToolError::OutcomeUnknown);
        }
        invocation_result(
            WorkflowInvocationResult::finished(
                identity,
                observed.0.clone(),
                observed.1.unwrap_or(Value::Null),
            ),
            observed.0 != ProgramOutcome::Completed,
        )
    }
}
async fn observe_foreground(
    cancellation: &CancellationToken,
    observation: Duration,
    receiver: &mut tokio::sync::watch::Receiver<Option<Finished>>,
    commands: &owner::Commands,
) -> Result<Option<(ProgramOutcome, Option<Value>)>, ProgramError> {
    let observed = tokio::select! {
        biased;
        () = cancellation.cancelled() => {
            if let Some(result) = owner::observe(commands, owner::Observation::CancelFromCreator, receiver).await {
                result?;
            }
            wait_finished(receiver).await
        }
        result = wait_finished(receiver) => result,
        () = tokio::time::sleep(observation) => {
            match owner::observe(commands, owner::Observation::Detach, receiver).await {
                Some(Ok(_)) => return Ok(None),
                Some(Err(error)) => return Err(error),
                None => wait_finished(receiver).await,
            }
        }
    }?;
    Ok(Some(observed))
}
fn invocation_result(
    value: WorkflowInvocationResult,
    failed: bool,
) -> rsi_tools_protocol::Result<ToolResult> {
    ToolResult::new(
        serde_json::to_value(value).map_err(failure)?,
        vec![],
        failed,
    )
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
            .map_err(turn_failure)?;
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
            .map_err(turn_failure)?;
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
type Finished = Result<(ProgramOutcome, Option<Value>), ProgramError>;
async fn wait_finished(receiver: &mut tokio::sync::watch::Receiver<Option<Finished>>) -> Finished {
    loop {
        if let Some(value) = receiver.borrow().clone() {
            return value;
        }
        receiver
            .changed()
            .await
            .map_err(|_| ProgramError::OutcomeUnknown)?;
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
    ) -> Result<Value, ProgramError> {
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
                let owner = self.0.clone();
                let agent = crate::runtime::contain(async move {
                    owner.agent(request).await.map_err(ProgramError::from)
                });
                tokio::pin!(agent);
                let result = tokio::select! {
                    biased;
                    () = cancellation.cancelled() => {
                        let owner = self.0.clone();
                        let cancel = crate::runtime::contain(async move { owner.cancel().await.map_err(ProgramError::from) });
                        // Child admission may own the lease needed by cancellation.
                        // Preserve and drive both original operations concurrently.
                        let (cancelled, settled) = tokio::join!(cancel, &mut agent);
                        if cancelled == Err(ProgramError::OutcomeUnknown)
                            || matches!(settled, Err(ProgramError::OutcomeUnknown)) {
                            return Err(ProgramError::OutcomeUnknown);
                        }
                        cancelled?;
                        return Err("workflow RPC cancelled".into());
                    }
                    result = &mut agent => result?,
                };
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
                    .map_err(ProgramError::from)?;
                Ok(Value::Null)
            }
            _ => Err("workflow admits only agent, phase and log RPCs".into()),
        }
    }
}

// Retire Jobs before publishing terminal state; attempt both even if either fails.
#[cfg(test)]
mod tests {
    #[tokio::test(start_paused = true)]
    async fn acknowledged_detach_refusal_returns_without_joining_run_completion() {
        for error in [
            ProgramError::Failed("known detach refusal".into()),
            ProgramError::Cancelled,
            ProgramError::Capacity,
            ProgramError::InvalidInput("invalid detach request".into()),
            ProgramError::OutcomeUnknown,
        ] {
            let cancellation = CancellationToken::new();
            let (commands, mut received) = tokio::sync::mpsc::channel(1);
            let (completion, mut finished) = tokio::sync::watch::channel(None);
            let observing = observe_foreground(
                &cancellation,
                Duration::from_secs(30),
                &mut finished,
                &commands,
            );
            tokio::pin!(observing);
            let (action, reply) = tokio::select! {
                result = &mut observing => panic!("unexpected completion {result:?}"),
                command = received.recv() => command.unwrap(),
            };
            assert!(matches!(action, owner::Observation::Detach));
            reply.send(Err(error.clone())).unwrap();
            tokio::select! {
                result = &mut observing => assert_eq!(result, Err(error)),
                () = tokio::time::sleep(Duration::from_secs(1)) => {
                    panic!("acknowledged refusal waited for the independent run")
                }
            }
            assert!(received.try_recv().is_err(), "one command, without retry");
            assert!(
                completion.borrow().is_none(),
                "run completion is still pending"
            );
        }
    }

    #[tokio::test(start_paused = true)]
    async fn foreground_observation_preserves_completion_and_successful_detachment() {
        for completed in [false, true] {
            let cancellation = CancellationToken::new();
            let (commands, mut received) = tokio::sync::mpsc::channel(1);
            let terminal = (ProgramOutcome::Completed, Some(json!(42)));
            let (_completion, mut finished) =
                tokio::sync::watch::channel(completed.then(|| Ok(terminal.clone())));
            let observing = observe_foreground(
                &cancellation,
                Duration::from_secs(30),
                &mut finished,
                &commands,
            );
            tokio::pin!(observing);
            if completed {
                assert_eq!(observing.await.unwrap(), Some(terminal));
                assert!(
                    received.try_recv().is_err(),
                    "completion needs no detach command"
                );
            } else {
                let (action, reply) = tokio::select! {
                    result = &mut observing => panic!("unexpected completion {result:?}"),
                    command = received.recv() => command.unwrap(),
                };
                assert!(matches!(action, owner::Observation::Detach));
                reply.send(Ok(true)).unwrap();
                assert!(
                    observing.await.unwrap().is_none(),
                    "the run remains independently owned"
                );
            }
        }
    }

    #[test]
    fn turn_refusals_preserve_tool_categories_and_uncertainty() {
        use rsi_agent_turn_protocol::TurnError;
        use rsi_tools_protocol::ToolError;
        for (turn, tool) in [
            (TurnError::Cancelled, ToolError::Cancelled),
            (TurnError::Capacity, ToolError::Capacity),
            (
                TurnError::Invalid("known refusal".into()),
                ToolError::InvalidInput("known refusal".into()),
            ),
            (
                TurnError::ExecutionOutcomeUnknown,
                ToolError::OutcomeUnknown,
            ),
            (
                TurnError::DomainOutcomeUnknown {
                    request_id: "request".into(),
                },
                ToolError::OutcomeUnknown,
            ),
        ] {
            assert_eq!(super::turn_failure(turn), tool);
        }
    }

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

    #[derive(Debug)]
    pub(super) struct RecordingRun {
        pub(super) jobs: Arc<dyn Jobs>,
        pub(super) scope: rsi_jobs::JobScopeAuthority,
        pub(super) durable: std::sync::Mutex<Option<ProgramOutcome>>,
        pub(super) fault: Option<rsi_agent_turn_protocol::TurnError>,
        pub(super) entered: CancellationToken,
        pub(super) release: Option<CancellationToken>,
        pub(super) calls: std::sync::atomic::AtomicUsize,
        pub(super) terminal: Option<ProgramOutcome>,
        pub(super) cancellation: CancellationToken,
    }
    #[async_trait]
    impl ProgramRun for RecordingRun {
        fn descriptor(&self) -> &rsi_agent_session_protocol::ProgramRunDescriptor {
            unreachable!()
        }
        fn cancellation(&self) -> CancellationToken {
            self.cancellation.clone()
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
            self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            self.cancellation.cancel();
            self.fault.clone().map_or(Ok(()), Err)
        }
        async fn agent(
            &self,
            _: ProgramAgentRequest,
        ) -> rsi_agent_turn_protocol::Result<rsi_agent_turn_protocol::ProgramAgentResult> {
            self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            self.entered.cancel();
            if let Some(release) = &self.release {
                release.cancelled().await;
            }
            Err(self.fault.clone().unwrap())
        }
        async fn progress(
            &self,
            _: Option<String>,
            _: String,
        ) -> rsi_agent_turn_protocol::Result<()> {
            self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Err(self.fault.clone().unwrap())
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
            if let Some(error) = &self.fault {
                return Err(error.clone());
            }
            Ok(self.terminal.clone().unwrap_or(outcome))
        }
    }

    #[tokio::test]
    async fn workflow_rpc_retains_typed_uncertainty_and_the_original_agent_on_cancel() {
        let meta = rsi_meta::Runtime::default();
        let plugin = meta
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
        let jobs = meta.root().lookup_local::<JobsContract>().unwrap();
        for fault in [
            rsi_agent_turn_protocol::TurnError::ExecutionOutcomeUnknown,
            rsi_agent_turn_protocol::TurnError::DomainOutcomeUnknown {
                request_id: "uncertain-domain".into(),
            },
        ] {
            let scope = jobs
                .acquire_scope(JobScopeId::new("test", ["rpc-uncertainty"]).unwrap())
                .unwrap();
            let release = CancellationToken::new();
            let run = Arc::new(RecordingRun {
                jobs: jobs.clone(),
                scope: scope.clone(),
                durable: std::sync::Mutex::new(None),
                fault: Some(fault),
                entered: CancellationToken::new(),
                release: Some(release.clone()),
                calls: std::sync::atomic::AtomicUsize::new(0),
                terminal: None,
                cancellation: CancellationToken::new(),
            });
            let rpc = WorkflowRpc(run.clone());
            for method in ["log", "phase"] {
                assert_eq!(
                    rpc.call(
                        method.into(),
                        json!({"name":"phase", "value":"progress"}),
                        CancellationToken::new()
                    )
                    .await,
                    Err(ProgramError::OutcomeUnknown)
                );
            }
            let cancel = CancellationToken::new();
            let future = rpc.call("agent".into(), json!({"message":"effect"}), cancel.clone());
            tokio::pin!(future);
            tokio::select! { result = &mut future => panic!("early settlement: {result:?}"), () = run.entered.cancelled() => {} }
            cancel.cancel();
            assert!(futures_util::FutureExt::now_or_never(future.as_mut()).is_none());
            assert_eq!(run.calls.load(std::sync::atomic::Ordering::SeqCst), 4);
            release.cancel();
            assert_eq!(future.await, Err(ProgramError::OutcomeUnknown));
            assert_eq!(
                run.calls.load(std::sync::atomic::Ordering::SeqCst),
                4,
                "agent was constructed once"
            );
            jobs.finalize_scope(&scope).await.unwrap();
        }
        assert!(plugin.dispose().await.is_clean());
        assert!(meta.shutdown().await.is_clean());
    }
}
