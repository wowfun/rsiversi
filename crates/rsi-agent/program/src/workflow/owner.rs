//! Private workflow ownership, independent of foreground observation.
use super::{Arguments, Finished, WorkflowRpc, bounded};
use crate::{ProgramError, ProgramRuntime};
use futures_util::{FutureExt, future::BoxFuture};
use rsi_agent_session_protocol::ProgramOutcome;
use rsi_agent_turn_protocol::{AgentCallerAuthority, ProgramRun};
use rsi_jobs::Jobs;
use rsi_tools_protocol::ToolExecution;
use serde_json::Value;
use std::sync::Arc;
use std::sync::{
    Mutex,
    atomic::{AtomicBool, Ordering},
};
use tokio::sync::{mpsc, oneshot, watch};
use tokio_util::sync::CancellationToken;

pub(super) enum Observation {
    Detach,
    CancelFromCreator,
}
type Command = (Observation, oneshot::Sender<Result<bool, ProgramError>>);
const COMMAND_OBSERVATION_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);
pub(super) type Commands = mpsc::Sender<Command>;
pub(super) type Ready = oneshot::Receiver<Result<(), ProgramError>>;

// Recovery survives consumption of drive's panicked future. This snapshot keeps
// cancellation/result observation without duplicating AdmittedProgram's start
// latch or its cancel-on-drop authority.
struct ProgramObservation {
    id: String,
    request: Arc<crate::runtime::Request>,
    result: watch::Receiver<Option<Result<Value, ProgramError>>>,
}
struct Ownership {
    run: Arc<dyn ProgramRun>,
    jobs: Arc<dyn Jobs>,
    scope: rsi_jobs::JobScopeAuthority,
    program: Mutex<Option<ProgramObservation>>,
    // Cleanup invocation markers publish with Release before the call; recovery's
    // Acquire loads prevent reinvocation. Admission is marked from its observed
    // result. Unknown is sticky and set before abort transfers the one future.
    // The active driver (or its recovery) writes these flags serially; Drop
    // marks unknown before the transferred future can be polled again. No
    // independent callback receives Ownership or writes a competing snapshot.
    accepted: AtomicBool,
    finalized: AtomicBool,
    terminal_started: AtomicBool,
    unknown: AtomicBool,
    aborted: CancellationToken,
}
impl Ownership {
    fn new(
        run: Arc<dyn ProgramRun>,
        jobs: Arc<dyn Jobs>,
        scope: rsi_jobs::JobScopeAuthority,
    ) -> Arc<Self> {
        Arc::new(Self {
            run,
            jobs,
            scope,
            program: Mutex::new(None),
            accepted: AtomicBool::new(false),
            finalized: AtomicBool::new(false),
            terminal_started: AtomicBool::new(false),
            unknown: AtomicBool::new(false),
            aborted: CancellationToken::new(),
        })
    }
    fn record<T>(&self, result: &Result<T, ProgramError>) {
        if matches!(result, Err(ProgramError::OutcomeUnknown)) {
            self.unknown.store(true, Ordering::Release);
        }
    }
    async fn accept(
        &self,
        call: impl std::future::Future<Output = Result<(), ProgramError>>,
    ) -> Result<(), ProgramError> {
        let accepted = if self.aborted.is_cancelled() {
            Err(ProgramError::Cancelled)
        } else {
            crate::runtime::contain(call).await
        };
        self.accepted.store(
            accepted.is_ok() || accepted == Err(ProgramError::OutcomeUnknown),
            Ordering::Release,
        );
        self.record(&accepted);
        accepted
    }
    fn cancel_program(&self) {
        if let Some(program) = self
            .program
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_ref()
        {
            program.request.cancel.cancel();
        }
    }
    async fn program_result(&self) -> Result<Value, ProgramError> {
        let result = self
            .program
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_ref()
            .map(|program| program.result.clone());
        match result {
            Some(result) => crate::runtime::wait_result(result).await,
            None => Err(ProgramError::OutcomeUnknown),
        }
    }
    async fn recover(self: Arc<Self>) -> Finished {
        self.unknown.store(true, Ordering::Release);
        self.cancel_program();
        let value = self.program_result().await;
        // Never reinvoke a single-use method that already began. Its Kernel
        // commit may still be owned; an unknown observation cannot prove closure.
        if self.finalized.load(Ordering::Acquire) || self.terminal_started.load(Ordering::Acquire) {
            return Err(ProgramError::OutcomeUnknown);
        }
        finish(&self, value, true).await
    }
}

// Aborting this task transfers the *same* boxed future, preserving method
// construction, admission and result ownership. Unwind is different: a panicked
// future is never repolled, and recovery only attempts not-yet-started cleanup.
// The driver is moved into a tracked task before its first poll. Its destructor
// registers the continuation before that parent releases its tracker token, so
// a closed tracker cannot become empty in the middle of this transfer.
struct Driver {
    future: Option<BoxFuture<'static, Finished>>,
    finished: Option<watch::Sender<Option<Finished>>>,
    ownership: Arc<Ownership>,
    tasks: tokio_util::task::TaskTracker,
    runtime: tokio::runtime::Handle,
}
impl Driver {
    async fn run(mut self) {
        let future = self.future.as_mut().expect("owned workflow future");
        let result = std::panic::AssertUnwindSafe(future).catch_unwind().await;
        let result = match result {
            Ok(result) => result,
            Err(payload) => {
                let _ = crate::runtime::contain_sync(|| drop(payload));
                self.future = Some(self.ownership.clone().recover().boxed());
                crate::runtime::contain(self.future.as_mut().expect("owned recovery future")).await
            }
        };
        if let Some(sender) = self.finished.take() {
            sender.send_replace(Some(result));
        }
        self.future.take();
    }
}
impl Drop for Driver {
    fn drop(&mut self) {
        let Some(future) = self.future.take() else {
            return;
        };
        self.ownership.unknown.store(true, Ordering::Release);
        self.ownership.aborted.cancel();
        self.ownership.cancel_program();
        let Some(sender) = self.finished.take() else {
            return;
        };
        let ownership = self.ownership.clone();
        // Orderly retirement keeps this runtime alive until retained owners join.
        self.tasks.spawn_on(
            async move {
                // No recursive restart guard. Callback construction/polling is
                // contained within the retained future; unexpected owner closure
                // remains typed uncertainty to all watch receivers.
                let result = crate::runtime::contain(future).await;
                let result = if ownership.unknown.load(Ordering::Acquire) {
                    Err(ProgramError::OutcomeUnknown)
                } else {
                    result
                };
                sender.send_replace(Some(result));
            },
            &self.runtime,
        );
    }
}

#[allow(clippy::too_many_arguments)] // One owner receives the complete already-prepared invocation.
pub(super) fn spawn(
    runtime: Arc<ProgramRuntime>,
    jobs: Arc<dyn Jobs>,
    run: Arc<dyn ProgramRun>,
    scope: rsi_jobs::JobScopeAuthority,
    args: Arguments,
    execution: ToolExecution,
    caller: Arc<AgentCallerAuthority>,
    process: rsi_tools_protocol::ToolProcess,
) -> (Ready, watch::Receiver<Option<Finished>>, Commands) {
    let ownership = Ownership::new(run, jobs, scope);
    let (ready, ready_rx) = oneshot::channel();
    let (finished, finished_rx) = watch::channel(None);
    let (commands, command_rx) = mpsc::channel(2);
    let tasks = runtime.tasks.clone();
    let future = drive(
        runtime,
        ownership.clone(),
        args,
        execution,
        caller,
        process,
        ready,
        command_rx,
    )
    .boxed();
    let driver = Driver {
        future: Some(future),
        finished: Some(finished),
        ownership,
        tasks: tasks.clone(),
        runtime: tokio::runtime::Handle::current(),
    };
    // track_future acquires its token synchronously before scheduling the driver.
    tasks.spawn(driver.run());
    (ready_rx, finished_rx, commands)
}

pub(super) async fn observe(
    commands: &Commands,
    action: Observation,
    finished: &mut watch::Receiver<Option<Finished>>,
) -> Option<Result<bool, ProgramError>> {
    let response = async {
        let (reply, result) = oneshot::channel();
        commands.send((action, reply)).await.ok()?;
        Some(result.await.unwrap_or(Err(ProgramError::OutcomeUnknown)))
    };
    tokio::select! {
        biased;
        _ = super::wait_finished(finished) => None,
        response = tokio::time::timeout(COMMAND_OBSERVATION_TIMEOUT, response) => {
            // Only the observation ends. A sent command remains in its original owner.
            response.unwrap_or(Some(Err(ProgramError::OutcomeUnknown)))
        }
    }
}

#[allow(clippy::too_many_arguments)] // Private driver receives one move-only invocation and its relays.
async fn drive(
    runtime: Arc<ProgramRuntime>,
    owner: Arc<Ownership>,
    args: Arguments,
    execution: ToolExecution,
    caller: Arc<AgentCallerAuthority>,
    process: rsi_tools_protocol::ToolProcess,
    ready: oneshot::Sender<Result<(), ProgramError>>,
    commands: mpsc::Receiver<Command>,
) -> Finished {
    let cancellation = match crate::runtime::contain_sync(|| owner.run.cancellation()) {
        Ok(cancellation) => cancellation,
        Err(error) => {
            owner.unknown.store(true, Ordering::Release);
            owner.finalized.store(true, Ordering::Release);
            let _ = crate::runtime::contain(async {
                owner
                    .jobs
                    .finalize_scope(&owner.scope)
                    .await
                    .map_err(ProgramError::from)
            })
            .await;
            let _ = ready.send(Err(error.clone()));
            return Err(error);
        }
    };
    let admitted = runtime
        .admit_owned(
            args.script,
            &execution,
            &owner.scope,
            Arc::new(WorkflowRpc(owner.run.clone())),
            cancellation.clone(),
            process,
        )
        .await;
    let mut program = match admitted {
        Ok(program) => program,
        Err(error) => {
            owner.record::<()>(&Err(error.clone()));
            owner.finalized.store(true, Ordering::Release);
            let cleanup = crate::runtime::contain(async {
                owner
                    .jobs
                    .finalize_scope(&owner.scope)
                    .await
                    .map_err(ProgramError::from)
            })
            .await;
            owner.record(&cleanup);
            let error = if owner.unknown.load(Ordering::Acquire) {
                ProgramError::OutcomeUnknown
            } else {
                error
            };
            let _ = ready.send(Err(error.clone()));
            return Err(error);
        }
    };
    *owner
        .program
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(ProgramObservation {
        id: program.id.clone(),
        request: program.request.clone(),
        result: program.result.clone(),
    });
    let accepted = owner
        .accept(async { owner.run.accept(&caller).await.map_err(ProgramError::from) })
        .await;
    let setup = async {
        accepted?;
        if owner.aborted.is_cancelled() {
            return Err(ProgramError::Cancelled);
        }
        crate::runtime::contain(async { owner.run.start().await.map_err(ProgramError::from) })
            .await?;
        if args.background {
            if owner.aborted.is_cancelled() {
                return Err(ProgramError::Cancelled);
            }
            crate::runtime::contain(async { owner.run.detach().await.map_err(ProgramError::from) })
                .await?;
        }
        if owner.aborted.is_cancelled() {
            return Err(ProgramError::OutcomeUnknown);
        }
        program.start().map_err(ProgramError::from)
    }
    .await;
    if let Err(error) = setup {
        owner.record::<()>(&Err(error.clone()));
        program.cancel();
        let value = program.result().await;
        let settled = finish(&owner, value, true).await;
        owner.record(&settled);
        let error = if owner.unknown.load(Ordering::Acquire) {
            ProgramError::OutcomeUnknown
        } else {
            error
        };
        let _ = ready.send(Err(error.clone()));
        return Err(error);
    }
    let _ = ready.send(Ok(()));
    let value = observe_program(&runtime, &owner, &program, &cancellation, commands).await;
    finish(&owner, value, false).await
}

async fn observe_program(
    runtime: &ProgramRuntime,
    owner: &Ownership,
    program: &crate::AdmittedProgram,
    cancellation: &CancellationToken,
    mut commands: mpsc::Receiver<Command>,
) -> Result<Value, ProgramError> {
    let mut observing = true;
    loop {
        tokio::select! {
            biased;
            () = owner.aborted.cancelled() => {
                program.cancel();
                let cancelled = crate::runtime::contain(async { owner.run.cancel().await.map_err(ProgramError::from) }).await;
                owner.record(&cancelled);
                break program.result().await;
            }
            () = runtime.cancellation.cancelled() => {
                program.cancel();
                let cancelled = crate::runtime::contain(async { owner.run.cancel().await.map_err(ProgramError::from) }).await;
                owner.record(&cancelled);
                break program.cancel_with("workflow runtime retired").await;
            }
            () = cancellation.cancelled() => break program.cancel_with("workflow cancelled").await,
            command = commands.recv(), if observing => {
                if let Some((action, reply)) = command {
                    let result = match action {
                        Observation::Detach => crate::runtime::contain(async { owner.run.detach().await.map(|()| true).map_err(ProgramError::from) }).await,
                        Observation::CancelFromCreator => crate::runtime::contain(async { owner.run.cancel_from_creator().await.map_err(ProgramError::from) }).await,
                    };
                    owner.record(&result);
                    let unknown = result == Err(ProgramError::OutcomeUnknown);
                    let _ = reply.send(result);
                    if unknown { program.cancel(); break program.result().await; }
                } else { observing = false; }
            }
            value = program.result() => break value,
        }
    }
}

async fn finish(
    owner: &Ownership,
    value: Result<Value, ProgramError>,
    setup_failed: bool,
) -> Finished {
    owner.record(&value);
    let id = owner
        .program
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .as_ref()
        .map(|program| program.id.clone());
    let reported = match id {
        Some(id) => {
            crate::runtime::contain(async {
                owner
                    .jobs
                    .wait(&owner.scope, &id, 0, 0)
                    .await
                    .map_err(ProgramError::from)
            })
            .await
        }
        None => Err(ProgramError::OutcomeUnknown),
    };
    owner.record(&reported);
    if matches!(&reported, Ok(read) if read.job.status == rsi_jobs::JobStatus::OutcomeUnknown) {
        owner.unknown.store(true, Ordering::Release);
    }
    owner.finalized.store(true, Ordering::Release);
    let finalized = crate::runtime::contain(async {
        owner
            .jobs
            .finalize_scope(&owner.scope)
            .await
            .map_err(ProgramError::from)
    })
    .await;
    owner.record(&finalized);
    if matches!(&finalized, Ok(report) if report.outcome_unknown) {
        owner.unknown.store(true, Ordering::Release);
    }
    if !owner.accepted.load(Ordering::Acquire) {
        return Err(if owner.unknown.load(Ordering::Acquire) {
            ProgramError::OutcomeUnknown
        } else {
            value
                .err()
                .unwrap_or_else(|| "workflow was not accepted".into())
        });
    }
    let cleanup = reported.map(|_| ()).and(finalized.map(|_| ()));
    let cancelled = crate::runtime::contain_sync(|| owner.run.cancellation().is_cancelled());
    owner.record(&cancelled);
    let outcome = if owner.unknown.load(Ordering::Acquire) {
        ProgramOutcome::Interrupted
    } else if setup_failed || cancelled.unwrap_or(true) {
        ProgramOutcome::Cancelled
    } else {
        match (&value, cleanup) {
            (_, Err(error)) => ProgramOutcome::Failed {
                code: "program.cleanup".into(),
                message: bounded(&error.to_string()),
            },
            (Ok(_), Ok(())) => ProgramOutcome::Completed,
            (Err(error), Ok(())) => ProgramOutcome::Failed {
                code: "program.execution".into(),
                message: bounded(&error.to_string()),
            },
        }
    };
    let script_value = value.ok().filter(|_| outcome == ProgramOutcome::Completed);
    owner.terminal_started.store(true, Ordering::Release);
    let terminal = crate::runtime::contain(async {
        owner
            .run
            .finish(outcome, script_value.clone())
            .await
            .map_err(ProgramError::from)
    })
    .await;
    owner.record(&terminal);
    if owner.unknown.load(Ordering::Acquire) {
        return Err(ProgramError::OutcomeUnknown);
    }
    let outcome = terminal?;
    Ok((
        outcome.clone(),
        script_value.filter(|_| outcome == ProgramOutcome::Completed),
    ))
}

#[cfg(test)]
mod tests {
    use super::super::tests::RecordingRun;
    use super::super::wait_finished;
    use super::*;
    use rsi_agent_turn_protocol::ProgramAgentRequest;
    use rsi_jobs::{JobScopeId, JobsContract};
    use serde_json::json;
    use std::sync::atomic::AtomicUsize;

    #[tokio::test]
    async fn aborted_before_acceptance_does_not_invoke_the_method_or_publish_a_terminal() {
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
        let scope = jobs
            .acquire_scope(JobScopeId::new("test", ["aborted-before-acceptance"]).unwrap())
            .unwrap();
        let run = Arc::new(RecordingRun {
            jobs: jobs.clone(),
            scope: scope.clone(),
            durable: Mutex::new(None),
            fault: None,
            entered: CancellationToken::new(),
            release: None,
            calls: AtomicUsize::new(0),
            terminal: None,
            cancellation: CancellationToken::new(),
        });
        let owner = Ownership::new(run.clone(), jobs.clone(), scope.clone());
        let program = settled_program(&owner);
        owner.aborted.cancel();
        let result = owner
            .accept(async {
                panic!("accept must not be constructed or polled after pre-admission abort")
            })
            .await;
        assert_eq!(result, Err(ProgramError::Cancelled));
        assert!(!owner.accepted.load(Ordering::Acquire));
        assert_eq!(
            finish(&owner, result.map(|()| Value::Null), true).await,
            Err(ProgramError::Cancelled)
        );
        assert!(run.durable.lock().unwrap().is_none());
        assert!(jobs.list(&scope).is_err());
        drop(program);
        assert!(plugin.dispose().await.is_clean());
        assert!(meta.shutdown().await.is_clean());
    }

    #[tokio::test]
    async fn abort_during_panic_recovery_retains_the_pending_result_and_cleanup() {
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
        let scope = jobs
            .acquire_scope(JobScopeId::new("test", ["panic-recovery-abort"]).unwrap())
            .unwrap();
        let run = Arc::new(RecordingRun {
            jobs: jobs.clone(),
            scope: scope.clone(),
            durable: Mutex::new(None),
            fault: None,
            entered: CancellationToken::new(),
            release: None,
            calls: AtomicUsize::new(0),
            terminal: None,
            cancellation: CancellationToken::new(),
        });
        let ownership = Ownership::new(run, jobs.clone(), scope.clone());
        let program = settled_program(&ownership);
        let completion = program.request.completion.lock().unwrap().take().unwrap();
        completion.send_replace(None);
        let tasks = tokio_util::task::TaskTracker::new();
        let (sender, mut observer) = watch::channel(None);
        let driver = Driver {
            future: Some(Box::pin(async { panic!("driver failure before cleanup") })),
            finished: Some(sender),
            ownership,
            tasks: tasks.clone(),
            runtime: tokio::runtime::Handle::current(),
        };
        let handle = tasks.spawn(driver.run());
        program.request.cancel.cancelled().await;
        tasks.close();
        handle.abort();
        assert!(handle.await.unwrap_err().is_cancelled());
        assert!(
            tasks.wait().now_or_never().is_none(),
            "recovery must retain the pending result owner"
        );
        assert!(
            jobs.list(&scope).is_ok(),
            "cleanup must still await the admitted result"
        );
        assert!(observer.borrow().is_none());
        completion.send_replace(Some(Ok(Value::Null)));
        assert_eq!(
            wait_finished(&mut observer).await,
            Err(ProgramError::OutcomeUnknown)
        );
        tasks.wait().await;
        assert!(
            jobs.list(&scope).is_err(),
            "recovery must finalize the original scope"
        );
        assert!(plugin.dispose().await.is_clean());
        assert!(meta.shutdown().await.is_clean());
    }

    #[tokio::test(start_paused = true)]
    async fn command_observation_deadline_bounds_queue_and_reply_without_discarding_command() {
        for queued in [false, true] {
            let (commands, mut receiver) = mpsc::channel(1);
            if queued {
                let (reply, _result) = oneshot::channel();
                commands.try_send((Observation::Detach, reply)).unwrap();
            }
            let (_completion, mut finished) = watch::channel(None);
            let observed = observe(&commands, Observation::Detach, &mut finished);
            tokio::pin!(observed);
            assert!(observed.as_mut().now_or_never().is_none());
            let admitted = if queued {
                None
            } else {
                Some(receiver.recv().await.unwrap())
            };
            tokio::time::advance(std::time::Duration::from_secs(5)).await;
            assert_eq!(
                observed.as_mut().now_or_never(),
                Some(Some(Err(ProgramError::OutcomeUnknown)))
            );
            if let Some((_, reply)) = admitted {
                // The admitted command still belongs to its receiver after the waiter timed out.
                assert!(reply.send(Ok(true)).is_err());
            } else {
                receiver.recv().await.unwrap();
                assert!(matches!(
                    receiver.try_recv(),
                    Err(mpsc::error::TryRecvError::Empty)
                ));
            }
        }
    }

    #[tokio::test(start_paused = true)]
    async fn command_observation_prioritizes_authoritative_completion_over_ack_or_deadline() {
        for completed_first in [true, false] {
            let (commands, mut receiver) = mpsc::channel(1);
            let terminal = Ok((ProgramOutcome::Completed, Some(json!(42))));
            let (completion, mut finished) =
                watch::channel(completed_first.then(|| terminal.clone()));
            let mut observer = Box::pin(observe(&commands, Observation::Detach, &mut finished));
            if !completed_first {
                assert!(observer.as_mut().now_or_never().is_none());
                let (_action, reply) = receiver.recv().await.unwrap();
                completion.send_replace(Some(terminal.clone()));
                reply.send(Err(ProgramError::OutcomeUnknown)).unwrap();
                tokio::time::advance(COMMAND_OBSERVATION_TIMEOUT).await;
            }
            assert_eq!(observer.await, None);
            assert_eq!(wait_finished(&mut finished).await, terminal);
            if completed_first {
                assert!(matches!(
                    receiver.try_recv(),
                    Err(mpsc::error::TryRecvError::Empty)
                ));
            }
        }
    }

    #[tokio::test]
    async fn abort_transfers_a_single_use_run_future_and_all_completion_observers() {
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
        let scope = jobs
            .acquire_scope(JobScopeId::new("test", ["owner-abort"]).unwrap())
            .unwrap();
        let release = CancellationToken::new();
        let run = Arc::new(RecordingRun {
            jobs: jobs.clone(),
            scope: scope.clone(),
            durable: Mutex::new(None),
            fault: Some(rsi_agent_turn_protocol::TurnError::ExecutionOutcomeUnknown),
            entered: CancellationToken::new(),
            release: Some(release.clone()),
            calls: AtomicUsize::new(0),
            terminal: None,
            cancellation: CancellationToken::new(),
        });
        let ownership = Ownership::new(run.clone(), jobs.clone(), scope.clone());
        let tasks = tokio_util::task::TaskTracker::new();
        let future_run = run.clone();
        let future_jobs = jobs.clone();
        let future_scope = scope.clone();
        let future = async move {
            let result = crate::runtime::contain(async {
                future_run
                    .agent(ProgramAgentRequest {
                        message: "effect".into(),
                        output_contract: None,
                        role: None,
                    })
                    .await
                    .map_err(ProgramError::from)
            })
            .await;
            future_jobs.finalize_scope(&future_scope).await.unwrap();
            result.map(|_| (ProgramOutcome::Completed, None))
        }
        .boxed();
        let (sender, mut first) = watch::channel(None);
        let mut second = first.clone();
        let driver = Driver {
            future: Some(future),
            finished: Some(sender),
            ownership,
            tasks: tasks.clone(),
            runtime: tokio::runtime::Handle::current(),
        };
        let handle = tasks.spawn(driver.run());
        run.entered.cancelled().await;
        tasks.close();
        handle.abort();
        assert!(handle.await.unwrap_err().is_cancelled());
        assert!(first.borrow().is_none() && second.borrow().is_none());
        assert!(
            tasks.wait().now_or_never().is_none(),
            "completion must still own the pending method"
        );
        assert_eq!(run.calls.load(Ordering::SeqCst), 1);
        release.cancel();
        assert_eq!(
            wait_finished(&mut first).await,
            Err(ProgramError::OutcomeUnknown)
        );
        assert_eq!(
            wait_finished(&mut second).await,
            Err(ProgramError::OutcomeUnknown)
        );
        tasks.wait().await;
        assert_eq!(
            run.calls.load(Ordering::SeqCst),
            1,
            "single-use method must not be reconstructed"
        );
        assert!(jobs.list(&scope).is_err());
        assert!(plugin.dispose().await.is_clean());
        assert!(meta.shutdown().await.is_clean());
    }

    fn settled_program(owner: &Ownership) -> crate::AdmittedProgram {
        let (sender, result) = watch::channel(Some(Ok(Value::Null)));
        let request = Arc::new(crate::runtime::Request {
            spec: Mutex::new(None),
            script: String::new(),
            definitions: Value::Null,
            rpc: Arc::new(WorkflowRpc(owner.run.clone())),
            start: CancellationToken::new(),
            cancel: CancellationToken::new(),
            cancelled_at_settlement: AtomicBool::new(false),
            outcome: result.clone(),
            completion: Mutex::new(Some(sender)),
        });
        *owner.program.lock().unwrap() = Some(ProgramObservation {
            id: "missing-job".into(),
            request: request.clone(),
            result: result.clone(),
        });
        crate::AdmittedProgram {
            id: "missing-job".into(),
            request,
            result,
            started: true,
        }
    }

    #[tokio::test]
    async fn observation_terminalizes_cleanup_failure_before_foreground_completion() {
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
        let scope = jobs
            .acquire_scope(JobScopeId::new("test", ["cleanup"]).unwrap())
            .unwrap();
        let run = Arc::new(RecordingRun {
            jobs: jobs.clone(),
            scope: scope.clone(),
            durable: Mutex::new(None),
            fault: None,
            entered: CancellationToken::new(),
            release: None,
            calls: AtomicUsize::new(0),
            terminal: None,
            cancellation: CancellationToken::new(),
        });
        let owner = Ownership::new(run.clone(), jobs.clone(), scope);
        owner.accepted.store(true, Ordering::Release);
        let program = settled_program(&owner);
        let runtime = ProgramRuntime {
            configuration: crate::ProgramConfiguration {
                node: "/unused-node".into(),
                environment: vec![],
            },
            jobs,
            tasks: tokio_util::task::TaskTracker::new(),
            cancellation: CancellationToken::new(),
        };
        let (commands, receiver) = mpsc::channel(2);
        // Dropping foreground observation must still reach actual Program settlement.
        drop(commands);
        let value = observe_program(&runtime, &owner, &program, &run.cancellation, receiver).await;
        assert_eq!(value, Ok(Value::Null));
        let (reported, value) = finish(&owner, value, false).await.unwrap();
        assert!(
            matches!(&reported, ProgramOutcome::Failed { code, .. } if code == "program.cleanup")
        );
        assert_eq!(*run.durable.lock().unwrap(), Some(reported));
        assert_eq!(
            value, None,
            "failed cleanup cannot expose successful script value"
        );
        assert!(plugin.dispose().await.is_clean());
        assert!(meta.shutdown().await.is_clean());
    }

    #[tokio::test]
    async fn retirement_observation_joins_settlement_and_preserves_cancel_uncertainty() {
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
            None,
            Some(rsi_agent_turn_protocol::TurnError::ExecutionOutcomeUnknown),
        ] {
            let scope = jobs
                .acquire_scope(JobScopeId::new("test", ["retirement"]).unwrap())
                .unwrap();
            let run = Arc::new(RecordingRun {
                jobs: jobs.clone(),
                scope: scope.clone(),
                durable: Mutex::new(None),
                fault: fault.clone(),
                entered: CancellationToken::new(),
                release: None,
                calls: AtomicUsize::new(0),
                terminal: None,
                cancellation: CancellationToken::new(),
            });
            let owner = Ownership::new(run.clone(), jobs.clone(), scope.clone());
            owner.accepted.store(true, Ordering::Release);
            let program = settled_program(&owner);
            let sender = program.request.completion.lock().unwrap().take().unwrap();
            sender.send_replace(None);
            let runtime = ProgramRuntime {
                configuration: crate::ProgramConfiguration {
                    node: "/unused-node".into(),
                    environment: vec![],
                },
                jobs: jobs.clone(),
                tasks: tokio_util::task::TaskTracker::new(),
                cancellation: CancellationToken::new(),
            };
            let (_commands, receiver) = mpsc::channel(2);
            runtime.cancellation.cancel();
            let observation =
                observe_program(&runtime, &owner, &program, &run.cancellation, receiver);
            tokio::pin!(observation);
            assert!(observation.as_mut().now_or_never().is_none());
            assert!(program.request.cancel.is_cancelled());
            assert_eq!(run.calls.load(Ordering::SeqCst), 1);
            assert!(
                jobs.list(&scope).is_ok(),
                "scope is retained through actual settlement"
            );
            assert!(run.durable.lock().unwrap().is_none());
            sender.send_replace(Some(Ok(Value::Null)));
            let value = observation.await;
            assert!(
                matches!(&value, Err(ProgramError::Failed(reason)) if reason == "workflow runtime retired")
            );
            let result = finish(&owner, value, false).await;
            if fault.is_some() {
                assert_eq!(result, Err(ProgramError::OutcomeUnknown));
                assert_eq!(
                    *run.durable.lock().unwrap(),
                    Some(ProgramOutcome::Interrupted)
                );
            } else {
                assert_eq!(result, Ok((ProgramOutcome::Cancelled, None)));
                assert_eq!(
                    *run.durable.lock().unwrap(),
                    Some(ProgramOutcome::Cancelled)
                );
            }
            assert!(jobs.list(&scope).is_err());
            assert_eq!(
                run.calls.load(Ordering::SeqCst),
                1,
                "retirement invokes cancel once"
            );
        }
        assert!(plugin.dispose().await.is_clean());
        assert!(meta.shutdown().await.is_clean());
    }

    #[tokio::test]
    async fn sticky_uncertainty_survives_cleanup_failure_and_existing_terminal_projection() {
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
        let scope = jobs
            .acquire_scope(JobScopeId::new("test", ["sticky-unknown"]).unwrap())
            .unwrap();
        let run = Arc::new(RecordingRun {
            jobs: jobs.clone(),
            scope: scope.clone(),
            durable: Mutex::new(None),
            fault: None,
            entered: CancellationToken::new(),
            release: None,
            calls: AtomicUsize::new(0),
            terminal: Some(ProgramOutcome::Cancelled),
            cancellation: CancellationToken::new(),
        });
        let owner = Ownership::new(run.clone(), jobs, scope);
        owner.accepted.store(true, Ordering::Release);
        owner.unknown.store(true, Ordering::Release);
        let result = finish(&owner, Ok(json!({"must_not_escape":true})), false).await;
        assert_eq!(result, Err(ProgramError::OutcomeUnknown));
        assert_eq!(
            *run.durable.lock().unwrap(),
            Some(ProgramOutcome::Interrupted)
        );
        let scope = owner
            .jobs
            .acquire_scope(JobScopeId::new("test", ["finish-unknown"]).unwrap())
            .unwrap();
        let terminal = Arc::new(RecordingRun {
            jobs: owner.jobs.clone(),
            scope: scope.clone(),
            durable: Mutex::new(None),
            fault: Some(rsi_agent_turn_protocol::TurnError::ExecutionOutcomeUnknown),
            entered: CancellationToken::new(),
            release: None,
            calls: AtomicUsize::new(0),
            terminal: None,
            cancellation: CancellationToken::new(),
        });
        let terminal_owner = Ownership::new(terminal.clone(), owner.jobs.clone(), scope);
        terminal_owner.accepted.store(true, Ordering::Release);
        settled_program(&terminal_owner);
        assert_eq!(
            finish(&terminal_owner, Ok(Value::Null), false).await,
            Err(ProgramError::OutcomeUnknown)
        );
        assert!(terminal_owner.unknown.load(Ordering::Acquire));
        let scope = owner
            .jobs
            .acquire_scope(JobScopeId::new("test", ["existing-terminal"]).unwrap())
            .unwrap();
        let terminal = Arc::new(RecordingRun {
            jobs: owner.jobs.clone(),
            scope: scope.clone(),
            durable: Mutex::new(None),
            fault: None,
            entered: CancellationToken::new(),
            release: None,
            calls: AtomicUsize::new(0),
            terminal: Some(ProgramOutcome::Cancelled),
            cancellation: CancellationToken::new(),
        });
        let terminal_owner = Ownership::new(terminal, owner.jobs.clone(), scope);
        terminal_owner.accepted.store(true, Ordering::Release);
        settled_program(&terminal_owner);
        assert_eq!(
            finish(&terminal_owner, Ok(json!({"unpublishable":true})), false).await,
            Ok((ProgramOutcome::Cancelled, None)),
            "existing terminal excludes a successful script value"
        );

        assert!(plugin.dispose().await.is_clean());
        assert!(meta.shutdown().await.is_clean());
    }
}
