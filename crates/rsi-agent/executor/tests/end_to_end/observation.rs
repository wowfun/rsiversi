use super::*;
use rsi_agent_turn_protocol::{
    ControlledWorkStatus, ExecutionInterval, ExecutionObservationEnd, ExecutionObserver,
    ExecutionObserverContract,
};
use std::time::Duration;

#[derive(Debug, Default)]
pub(super) struct Observer {
    pub entered: Notify,
    pub release: CancellationToken,
    pub ended: Notify,
    pub result: Mutex<Option<ExecutionObservationEnd>>,
    pub block_end: AtomicBool,
    pub end_entered: Notify,
    pub end_release: CancellationToken,
    pub block_admission: AtomicBool,
    pub admission_stop: Mutex<Option<CancellationToken>>,
    pub admission_entered: Notify,
}
#[async_trait]
impl ExecutionInterval for Observer {
    async fn begin(&self, stop: CancellationToken) {
        self.entered.notify_one();
        tokio::select! { ()=self.release.cancelled()=>{}, ()=stop.cancelled()=>{} }
    }
    async fn end(&self, evidence: ExecutionObservationEnd, stop: CancellationToken) {
        *self.result.lock().unwrap() = Some(evidence);
        self.end_entered.notify_one();
        if self.block_end.load(Ordering::Acquire) {
            tokio::select! { ()=self.end_release.cancelled()=>{}, ()=stop.cancelled()=>{} }
        }
        self.ended.notify_one();
    }
}
#[derive(Debug)]
struct Observe(Arc<Observer>);
#[async_trait]
impl ExecutionObserver for Observe {
    async fn observe(
        &self,
        _: rsi_agent_turn_protocol::ExecutionObservationStart,
        stop: CancellationToken,
    ) -> Result<Arc<dyn ExecutionInterval>, String> {
        if self.0.block_admission.load(Ordering::Acquire) {
            *self.0.admission_stop.lock().unwrap() = Some(stop);
            self.0.admission_entered.notify_one();
            std::future::pending::<()>().await;
        }
        Ok(self.0.clone())
    }
}
#[derive(Debug)]
struct Factory(Arc<Observer>);
#[async_trait]
impl PluginFactory for Factory {
    fn prepare(&self, config: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        Ok(PreparedActivation::new(config.clone()))
    }
    async fn activate(&self, plan: ActivationPlan) -> rsi_meta::Result<()> {
        let service: Arc<dyn ExecutionObserver> = Arc::new(Observe(self.0.clone()));
        let supply = plan
            .context()
            .provide_local::<ExecutionObserverContract>(service)?;
        plan.defer(
            "withdraw execution observer",
            Box::new(move || {
                Box::pin(async move {
                    drop(supply);
                    Ok(())
                })
            }),
        )
    }
}
pub(super) async fn install(stack: &BaseStack) -> (Arc<Observer>, FiberHandle) {
    let observer = Arc::new(Observer::default());
    let fiber = activate_fixture(
        &stack.runtime,
        "fixture.observer",
        "1",
        Arc::new(Factory(observer.clone())),
    )
    .await;
    (observer, fiber)
}

#[derive(Debug)]
struct Finalizer {
    entered: Arc<Notify>,
    release: CancellationToken,
}

#[tokio::test(start_paused = true)]
async fn observation_admission_deadline_cancels_before_any_model_effect() {
    let stack = BaseStack::activate().await;
    let (observer, owner) = install(&stack).await;
    observer.block_admission.store(true, Ordering::Release);
    let starts = Arc::new(AtomicUsize::new(0));
    let language = stack
        .activate_language(
            "test.language.admission",
            Arc::new(LanguageFixture {
                outcomes: Mutex::new(VecDeque::new()),
                requests: Mutex::new(vec![]),
                starts: starts.clone(),
                store: stack.store.clone(),
                retry_policy: RetryPolicy::default(),
            }),
        )
        .await;
    let executor = stack
        .activate_executor_with_config(json!({
            "executor_id":"observation-admission", "observe_execution":true
        }))
        .await;
    let turns = stack
        .runtime
        .root()
        .lookup_local::<TurnServiceContract>()
        .unwrap();
    let receipt = stack
        .submit_fresh(&turns, "observed-admission", "first")
        .await;
    observer.admission_entered.notified().await;
    tokio::time::advance(Duration::from_secs(31)).await;
    let outcome = wait_for_outcome(&turns, &receipt).await;
    assert!(
        matches!(outcome, TurnOutcome::Failed { ref code, .. } if code == "observation.admission")
    );
    assert_eq!(starts.load(Ordering::Acquire), 0);
    assert!(
        observer
            .admission_stop
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .is_cancelled()
    );
    assert!(executor.dispose().await.is_clean());
    assert!(owner.dispose().await.is_clean());
    assert!(language.dispose().await.is_clean());
    stack.dispose_services().await;
}

#[tokio::test]
async fn blocked_observation_end_does_not_hold_the_only_execution_lane() {
    let stack = BaseStack::activate().await;
    let (observer, owner) = install(&stack).await;
    observer.release.cancel();
    observer.block_end.store(true, Ordering::Release);
    let second_entered = Arc::new(Notify::new());
    let second_release = Arc::new(Notify::new());
    let language = stack
        .activate_language(
            "test.language.observation-tail",
            Arc::new(LanguageFixture {
                outcomes: Mutex::new(VecDeque::from([
                    StartOutcome::Stream(answer_script()),
                    gated_answer(&second_entered, &second_release),
                ])),
                requests: Mutex::new(vec![]),
                starts: Arc::new(AtomicUsize::new(0)),
                store: stack.store.clone(),
                retry_policy: RetryPolicy::default(),
            }),
        )
        .await;
    let executor = stack
        .activate_executor_with_config(json!({
            "executor_id":"observation-tail", "maximum_active_turns":1, "observe_execution":true
        }))
        .await;
    let turns = stack
        .runtime
        .root()
        .lookup_local::<TurnServiceContract>()
        .unwrap();
    let first = stack.submit_fresh(&turns, "observed-first", "first").await;
    observer.end_entered.notified().await;
    assert_eq!(
        wait_for_outcome(&turns, &first).await,
        TurnOutcome::Completed
    );
    let second = stack
        .submit_fresh(&turns, "observed-second", "second")
        .await;
    let progressed = tokio::time::timeout(Duration::from_secs(2), second_entered.notified())
        .await
        .is_ok();
    observer.end_release.cancel();
    if !progressed {
        tokio::time::timeout(Duration::from_secs(2), second_entered.notified())
            .await
            .unwrap();
    }
    second_release.notify_one();
    assert_eq!(
        wait_for_outcome(&turns, &second).await,
        TurnOutcome::Completed
    );
    assert!(executor.dispose().await.is_clean());
    assert!(owner.dispose().await.is_clean());
    assert!(language.dispose().await.is_clean());
    stack.dispose_services().await;
    assert!(
        progressed,
        "a completed Turn's observer held the execution lane"
    );
}
#[async_trait]
impl TurnFinalizer for Finalizer {
    async fn finalize(
        &self,
        _: &TurnFinalizationContext,
    ) -> FinalizationResult<TurnFinalizationReport> {
        self.entered.notify_one();
        self.release.cancelled().await;
        Ok(TurnFinalizationReport::default())
    }
}

#[tokio::test]
async fn executor_awaits_baseline_before_model_effects_and_end_after_finalization() {
    let stack = BaseStack::activate().await;
    let (observer, owner) = install(&stack).await;
    let starts = Arc::new(AtomicUsize::new(0));
    let language = stack
        .activate_language(
            "test.language.observed",
            Arc::new(LanguageFixture {
                outcomes: Mutex::new(VecDeque::from([StartOutcome::Stream(answer_script())])),
                requests: Mutex::new(vec![]),
                starts: starts.clone(),
                store: stack.store.clone(),
                retry_policy: RetryPolicy::default(),
            }),
        )
        .await;
    let (finalizer_owner, context) =
        rsi_agent_testkit::activate_contribution_owner(&stack.runtime.root())
            .await
            .unwrap();
    let entered = Arc::new(Notify::new());
    let release = CancellationToken::new();
    let lease = stack
        .runtime
        .root()
        .lookup_local::<TurnFinalizationContract>()
        .unwrap()
        .register(
            &context.registration_context().unwrap(),
            "ordered-observer-finalizer".into(),
            Arc::new(Finalizer {
                entered: entered.clone(),
                release: release.clone(),
            }),
        )
        .unwrap();
    let executor = stack
        .activate_executor_with_config(json!({"executor_id":"observed","observe_execution":true}))
        .await;
    let turns = stack
        .runtime
        .root()
        .lookup_local::<TurnServiceContract>()
        .unwrap();
    let submitted = turns
        .submit(SubmitTurn {
            reasoning_effort: None,
            turn_id: client_turn_id(),
            session: stack.fresh(header()).await,
            text: "capture order".into(),
            model: None,
            sandbox: None,
        })
        .await
        .unwrap();
    observer.entered.notified().await;
    assert_eq!(
        starts.load(Ordering::SeqCst),
        0,
        "baseline must complete before dispatch"
    );
    assert!(observer.result.lock().unwrap().is_none());
    observer.release.cancel();
    entered.notified().await;
    assert_eq!(starts.load(Ordering::SeqCst), 1);
    assert!(
        observer.result.lock().unwrap().is_none(),
        "end cannot race a parallel finalizer"
    );
    release.cancel();
    tokio::time::timeout(std::time::Duration::from_secs(2), observer.ended.notified())
        .await
        .unwrap();
    assert_eq!(
        *observer.result.lock().unwrap(),
        Some(ExecutionObservationEnd {
            begin_completed: true,
            controlled_work: ControlledWorkStatus::Settled
        })
    );
    assert_eq!(
        turns
            .outcome(&submitted.session_id, &submitted.turn_id)
            .await
            .unwrap(),
        Some(TurnOutcome::Completed)
    );
    assert!(executor.dispose().await.is_clean());
    drop(lease);
    assert!(finalizer_owner.dispose().await.is_clean());
    assert!(owner.dispose().await.is_clean());
    assert!(language.dispose().await.is_clean());
    stack.dispose_services().await;
}

#[tokio::test(start_paused = true)]
async fn predecessor_observation_conflict_does_not_fail_a_valid_turn() {
    let stack = BaseStack::activate().await;
    let entered = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let fixture = Arc::new(LanguageFixture {
        outcomes: Mutex::new(VecDeque::from([gated_answer(&entered, &release)])),
        requests: Mutex::new(vec![]),
        starts: Arc::new(AtomicUsize::new(0)),
        store: stack.store.clone(),
        retry_policy: RetryPolicy::default(),
    });
    let language = stack
        .activate_language("test.language.observation-conflict", fixture.clone())
        .await;
    let turns = stack
        .runtime
        .root()
        .lookup_local::<TurnServiceContract>()
        .unwrap();
    let execution = stack
        .runtime
        .root()
        .lookup_local::<TurnExecutionContract>()
        .unwrap();
    let lease = execution.register("predecessor".into()).unwrap();
    let submitted = stack
        .submit_fresh(
            &turns,
            "session-observation-conflict",
            "retry observation admission",
        )
        .await;
    let claim = execution
        .claim("predecessor", CancellationToken::new())
        .await
        .unwrap()
        .unwrap();
    let (observation, reporter) = rsi_agent_turn_protocol::ControlledWork::new();
    execution
        .publish_controlled_work(&claim, observation)
        .unwrap();
    execution.release(&claim).unwrap();
    drop(lease);
    let executor = stack
        .activate_executor_with_config(json!({"executor_id":"observation-conflict"}))
        .await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(
        turns
            .outcome(&submitted.session_id, &submitted.turn_id)
            .await
            .unwrap(),
        None
    );
    assert_eq!(fixture.starts.load(Ordering::Acquire), 0);
    reporter.finish(false);
    tokio::time::timeout(Duration::from_secs(2), entered.notified())
        .await
        .unwrap();
    release.notify_one();
    assert_eq!(
        wait_for_outcome(&turns, &submitted).await,
        TurnOutcome::Completed
    );
    drop(execution);
    drop(turns);
    stack.dispose(language, executor).await;
}
