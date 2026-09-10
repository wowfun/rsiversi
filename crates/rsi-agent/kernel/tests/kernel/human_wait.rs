use super::*;
use rsi_agent_kernel::KernelError;
use rsi_tools_protocol::{ToolLaneParkingAuthority, ToolLaneParkingService};
use std::sync::atomic::AtomicU64;
use std::time::Duration;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

#[derive(Debug)]
struct HumanClock(AtomicU64);
impl Clock for HumanClock {
    fn now_ms(&self) -> u64 {
        self.0.load(Ordering::SeqCst)
    }
}

#[derive(Debug)]
struct Lane {
    pool: Arc<Semaphore>,
    permit: Mutex<Option<OwnedSemaphorePermit>>,
    resumed: Notify,
}
#[async_trait]
impl ToolLaneParkingService for Lane {
    async fn park(&self) -> rsi_tools_protocol::Result<()> {
        self.permit.lock().unwrap().take();
        Ok(())
    }
    async fn resume(&self, cancellation: CancellationToken) -> rsi_tools_protocol::Result<()> {
        let result = tokio::select! {
            biased;
            () = cancellation.cancelled() => Err(ToolError::Cancelled),
            permit = self.pool.clone().acquire_owned() => {
                *self.permit.lock().unwrap() = Some(permit.map_err(|_| ToolError::ShuttingDown)?); Ok(())
            }
        };
        self.resumed.notify_one();
        result
    }
}

#[tokio::test(start_paused = true)]
async fn permanent_wait_resume_faults_pause_the_session_and_release_owned_cleanup() {
    for fault in [
        WaitResumeFault::MissingActivation,
        WaitResumeFault::Read(StoreError::Corrupt("injected corrupt activation".into())),
        WaitResumeFault::Commit(StoreError::Invalid("injected invalid resume".into())),
    ] {
        let memory = Arc::new(MemoryStore::new());
        let store = Arc::new(FactReadRaceStore::new(memory.clone()));
        let kernel =
            AgentKernel::recover_with_clock(store.clone(), composition(), Arc::new(FixedClock))
                .await
                .unwrap();
        let workers = kernel.start_workers();
        kernel
            .submit_message(SubmitMessage {
                session: fresh(header("permanent-wait")),
                message: mailbox_message("permanent-wait"),
                delivery: rsi_agent_session_protocol::MessageDelivery::NextTurn,
            })
            .await
            .unwrap();
        let _executor = kernel.register("permanent-wait".into()).unwrap();
        let claim = kernel
            .claim("permanent-wait", CancellationToken::new())
            .await
            .unwrap()
            .unwrap();
        let pool = Arc::new(Semaphore::new(1));
        let lane = Arc::new(Lane {
            permit: Mutex::new(Some(pool.clone().acquire_owned().await.unwrap())),
            pool,
            resumed: Notify::new(),
        });
        let waiting = kernel
            .park_human_wait(&claim, ToolLaneParkingAuthority::new(lane))
            .await
            .unwrap();
        *store.wait_resume_fault.lock().unwrap() = Some(fault);
        let result = tokio::time::timeout(
            Duration::from_secs(1),
            waiting.resume(CancellationToken::new()),
        )
        .await;
        assert!(
            matches!(result, Ok(Err(_))),
            "permanent wait failure kept retrying: {result:?}"
        );
        assert_eq!(
            memory
                .inspect_session(claim.session_id())
                .await
                .unwrap()
                .activation_phase,
            Some(StoreActivationPhase::Parked)
        );
        kernel.release(&claim).unwrap();
        let cancelled = CancellationToken::new();
        cancelled.cancel();
        assert!(
            kernel
                .claim("permanent-wait", cancelled)
                .await
                .unwrap()
                .is_none(),
            "failed durable wait was admitted for another model run"
        );
        let stopped = tokio::time::timeout(Duration::from_secs(1), kernel.shutdown(workers)).await;
        assert!(
            matches!(&stopped, Ok(Err(KernelError::Shutdown(message))) if message.contains("retained mutation failed")),
            "shutdown must report the failure without retaining a retry task: {stopped:?}"
        );
        drop(kernel);
        let recovered = super::kernel(memory).await;
        assert!(matches!(
            recovered
                .outcome(claim.session_id(), claim.turn_id())
                .await
                .unwrap(),
            Some(TurnOutcome::Interrupted { .. })
        ));
    }
}

#[tokio::test(start_paused = true)]
async fn agent_wait_resume_retries_failures_and_lost_acknowledgements() {
    for lost_ack in [false, true] {
        let memory = Arc::new(MemoryStore::new());
        let store = Arc::new(FactReadRaceStore::new(memory.clone()));
        let kernel =
            AgentKernel::recover_with_clock(store.clone(), composition(), Arc::new(FixedClock))
                .await
                .unwrap();
        let workers = kernel.start_workers();
        let (parent, child, _lease) =
            super::agent_lifecycle::active_parent_and_child(&kernel).await;
        store
            .fail_wait_resumes
            .store(usize::from(!lost_ack), Ordering::Release);
        store
            .fail_wait_resume_after_apply
            .store(lost_ack, Ordering::Release);
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(6),
            kernel.wait_agent(
                &kernel.agent_caller(&parent).unwrap(),
                std::time::Duration::from_millis(1),
                CancellationToken::new(),
            ),
        )
        .await
        .unwrap();
        assert!(
            matches!(result, Ok(AgentWaitResult::TimedOut)),
            "a transient resume fault stranded the Agent wait: {result:?}"
        );
        assert_eq!(
            memory
                .inspect_session(parent.session_id())
                .await
                .unwrap()
                .activation_phase,
            Some(StoreActivationPhase::Running)
        );
        let controls = memory
            .read_controls(parent.session_id(), 0, 256)
            .await
            .unwrap();
        assert_eq!(
            controls
                .records
                .iter()
                .filter(|record| matches!(
                    record.body(),
                    AgentControlRecordBody::WaitResumed { .. }
                ))
                .count(),
            1
        );
        kernel
            .finish_turn(&child, &TurnOutcome::Completed)
            .await
            .unwrap();
        kernel
            .finish_turn(&parent, &TurnOutcome::Completed)
            .await
            .unwrap();
        kernel.shutdown(workers).await.unwrap();
    }
}

#[derive(Debug)]
struct FailedParking(Arc<FactReadRaceStore>);

#[tokio::test(start_paused = true)]
async fn rejected_overlapping_agent_park_cannot_resume_the_original_wait() {
    let memory = Arc::new(MemoryStore::new());
    let kernel = super::kernel(memory.clone()).await;
    let workers = kernel.start_workers();
    let (parent, child, _lease) = super::agent_lifecycle::active_parent_and_child(&kernel).await;
    let cancellation = CancellationToken::new();
    let waiting = tokio::spawn({
        let kernel = kernel.clone();
        let caller = kernel.agent_caller(&parent).unwrap();
        let cancellation = cancellation.clone();
        async move {
            kernel
                .wait_agent(&caller, Duration::from_hours(1), cancellation)
                .await
        }
    });
    while memory
        .active_activation(parent.session_id())
        .await
        .unwrap()
        .unwrap()
        .phase
        != StoreActivationPhase::Parked
    {
        tokio::task::yield_now().await;
    }
    assert!(
        kernel
            .wait_agent(
                &kernel.agent_caller(&parent).unwrap(),
                Duration::from_millis(1),
                CancellationToken::new()
            )
            .await
            .is_err()
    );
    assert_eq!(
        memory
            .active_activation(parent.session_id())
            .await
            .unwrap()
            .unwrap()
            .phase,
        StoreActivationPhase::Parked,
        "a rejected second park resumed another wait's activation"
    );
    cancellation.cancel();
    assert!(waiting.await.unwrap().is_err());
    kernel
        .finish_turn(&child, &TurnOutcome::Completed)
        .await
        .unwrap();
    kernel
        .finish_turn(&parent, &TurnOutcome::Completed)
        .await
        .unwrap();
    kernel.shutdown(workers).await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn lost_park_ack_is_reconciled_and_preserves_the_original_error() {
    let memory = Arc::new(MemoryStore::new());
    let store = Arc::new(FactReadRaceStore::new(memory.clone()));
    let kernel =
        AgentKernel::recover_with_clock(store.clone(), composition(), Arc::new(FixedClock))
            .await
            .unwrap();
    let workers = kernel.start_workers();
    let (parent, child, _lease) = super::agent_lifecycle::active_parent_and_child(&kernel).await;
    store
        .fail_wait_park_after_apply
        .store(true, Ordering::Release);
    let result = kernel
        .wait_agent(
            &kernel.agent_caller(&parent).unwrap(),
            Duration::from_millis(1),
            CancellationToken::new(),
        )
        .await;
    assert_eq!(
        memory
            .active_activation(parent.session_id())
            .await
            .unwrap()
            .unwrap()
            .phase,
        StoreActivationPhase::Running,
        "lost park acknowledgement left the live Turn durably parked"
    );
    assert!(
        matches!(result, Err(TurnError::Store(ref error)) if error.contains("lost wait park acknowledgement")),
        "park cause was masked: {result:?}"
    );
    let controls = memory
        .read_controls(parent.session_id(), 0, 256)
        .await
        .unwrap();
    assert_eq!(
        controls
            .records
            .iter()
            .filter(|record| matches!(record.body(), AgentControlRecordBody::WaitResumed { .. }))
            .count(),
        1
    );
    kernel
        .finish_turn(&child, &TurnOutcome::Completed)
        .await
        .unwrap();
    kernel
        .finish_turn(&parent, &TurnOutcome::Completed)
        .await
        .unwrap();
    kernel.shutdown(workers).await.unwrap();
}

#[async_trait]
impl ToolLaneParkingService for FailedParking {
    async fn park(&self) -> rsi_tools_protocol::Result<()> {
        self.0
            .fail_wait_resumes
            .store(usize::MAX, Ordering::Release);
        Err(ToolError::Execution(
            "injected executor parking failure".into(),
        ))
    }
    async fn resume(&self, _: CancellationToken) -> rsi_tools_protocol::Result<()> {
        panic!("failed parking never acquired a parked executor lane")
    }
}

#[tokio::test(start_paused = true)]
async fn failed_human_parking_has_a_bounded_waiter_and_retains_its_durable_cleanup() {
    let memory = Arc::new(MemoryStore::new());
    let store = Arc::new(FactReadRaceStore::new(memory.clone()));
    let kernel =
        AgentKernel::recover_with_clock(store.clone(), composition(), Arc::new(FixedClock))
            .await
            .unwrap();
    let workers = kernel.start_workers();
    let (parent, child, _lease) = super::agent_lifecycle::active_parent_and_child(&kernel).await;
    let outcome = tokio::time::timeout(
        std::time::Duration::from_secs(61),
        kernel.park_human_wait(
            &parent,
            ToolLaneParkingAuthority::new(Arc::new(FailedParking(store.clone()))),
        ),
    )
    .await
    .expect("park failure cleanup held the caller past its durability deadline");
    assert!(matches!(outcome, Err(TurnError::Flush(_))));
    assert_eq!(
        memory
            .inspect_session(parent.session_id())
            .await
            .unwrap()
            .activation_phase,
        Some(StoreActivationPhase::Parked)
    );
    store.fail_wait_resumes.store(0, Ordering::Release);
    tokio::time::timeout(std::time::Duration::from_secs(6), async {
        while memory
            .inspect_session(parent.session_id())
            .await
            .unwrap()
            .activation_phase
            == Some(StoreActivationPhase::Parked)
        {
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
    })
    .await
    .unwrap();
    kernel
        .finish_turn(&child, &TurnOutcome::Completed)
        .await
        .unwrap();
    kernel
        .finish_turn(&parent, &TurnOutcome::Completed)
        .await
        .unwrap();
    kernel.shutdown(workers).await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn human_resume_retries_storage_failures_without_stranding_or_duplicating_the_wait() {
    for lost_acknowledgement in [false, true] {
        let memory = Arc::new(MemoryStore::new());
        let store = Arc::new(FactReadRaceStore::new(memory.clone()));
        let kernel =
            AgentKernel::recover_with_clock(store.clone(), composition(), Arc::new(FixedClock))
                .await
                .unwrap();
        let workers = kernel.start_workers();
        kernel
            .submit_message(SubmitMessage {
                session: fresh(header("human-retry")),
                message: mailbox_message("human"),
                delivery: rsi_agent_session_protocol::MessageDelivery::NextTurn,
            })
            .await
            .unwrap();
        let _lease = kernel.register("human-worker".into()).unwrap();
        let claim = kernel
            .claim("human-worker", CancellationToken::new())
            .await
            .unwrap()
            .unwrap();
        let pool = Arc::new(Semaphore::new(1));
        let lane = Arc::new(Lane {
            pool: pool.clone(),
            permit: Mutex::new(Some(pool.clone().acquire_owned().await.unwrap())),
            resumed: Notify::new(),
        });
        let waiting = kernel
            .park_human_wait(&claim, ToolLaneParkingAuthority::new(lane))
            .await
            .unwrap();
        store
            .fail_wait_resumes
            .store(usize::from(!lost_acknowledgement), Ordering::Release);
        store
            .fail_wait_resume_after_apply
            .store(lost_acknowledgement, Ordering::Release);
        tokio::time::timeout(
            std::time::Duration::from_secs(6),
            waiting.resume(CancellationToken::new()),
        )
        .await
        .unwrap()
        .expect("transient storage failure stranded the human wait");
        assert_eq!(
            memory
                .inspect_session(claim.session_id())
                .await
                .unwrap()
                .activation_phase,
            Some(StoreActivationPhase::Running)
        );
        let controls = memory
            .read_controls(claim.session_id(), 0, 256)
            .await
            .unwrap();
        assert_eq!(
            controls
                .records
                .iter()
                .filter(|control| matches!(
                    control.body(),
                    AgentControlRecordBody::WaitResumed { .. }
                ))
                .count(),
            1
        );
        kernel
            .finish_turn(&claim, &TurnOutcome::Completed)
            .await
            .unwrap();
        kernel.shutdown(workers).await.unwrap();
    }
}

#[tokio::test(start_paused = true)]
async fn shutdown_timeout_retains_failed_human_resume_until_storage_recovers() {
    let memory = Arc::new(MemoryStore::new());
    let store = Arc::new(FactReadRaceStore::new(memory.clone()));
    let kernel =
        AgentKernel::recover_with_clock(store.clone(), composition(), Arc::new(FixedClock))
            .await
            .unwrap();
    let workers = kernel.start_workers();
    kernel
        .submit_message(SubmitMessage {
            session: fresh(header("human-shutdown-retry")),
            message: mailbox_message("human"),
            delivery: rsi_agent_session_protocol::MessageDelivery::NextTurn,
        })
        .await
        .unwrap();
    let _lease = kernel.register("human-worker".into()).unwrap();
    let claim = kernel
        .claim("human-worker", CancellationToken::new())
        .await
        .unwrap()
        .unwrap();
    let pool = Arc::new(Semaphore::new(1));
    let lane = Arc::new(Lane {
        pool: pool.clone(),
        permit: Mutex::new(Some(pool.clone().acquire_owned().await.unwrap())),
        resumed: Notify::new(),
    });
    let waiting = kernel
        .park_human_wait(&claim, ToolLaneParkingAuthority::new(lane))
        .await
        .unwrap();
    store.fail_wait_resumes.store(usize::MAX, Ordering::Release);
    drop(waiting);
    assert!(
        kernel.shutdown(workers).await.is_err(),
        "shutdown cannot finish while the durable wait is still parked"
    );
    assert_eq!(
        memory
            .inspect_session(claim.session_id())
            .await
            .unwrap()
            .activation_phase,
        Some(StoreActivationPhase::Parked)
    );
    assert!((1..=8).contains(&store.wait_resume_attempts.load(Ordering::Acquire)));
    store.fail_wait_resumes.store(0, Ordering::Release);
    tokio::time::timeout(std::time::Duration::from_secs(6), async {
        while memory
            .inspect_session(claim.session_id())
            .await
            .unwrap()
            .activation_phase
            == Some(StoreActivationPhase::Parked)
        {
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("cleanup ownership was dropped with the failed resume");
    assert_eq!(pool.available_permits(), 1);
}

#[tokio::test(start_paused = true)]
async fn timed_out_or_aborted_resume_keeps_claim_retirement_owned_until_repaired() {
    for abort_waiter in [false, true] {
        let memory = Arc::new(MemoryStore::new());
        let store = Arc::new(FactReadRaceStore::new(memory.clone()));
        let kernel =
            AgentKernel::recover_with_clock(store.clone(), composition(), Arc::new(FixedClock))
                .await
                .unwrap();
        let workers = kernel.start_workers();
        kernel
            .submit_message(SubmitMessage {
                session: fresh(header("human-retained-retry")),
                message: mailbox_message("human"),
                delivery: rsi_agent_session_protocol::MessageDelivery::NextTurn,
            })
            .await
            .unwrap();
        let _lease = kernel.register("human-worker".into()).unwrap();
        let claim = kernel
            .claim("human-worker", CancellationToken::new())
            .await
            .unwrap()
            .unwrap();
        let pool = Arc::new(Semaphore::new(1));
        let lane = Arc::new(Lane {
            pool: pool.clone(),
            permit: Mutex::new(Some(pool.clone().acquire_owned().await.unwrap())),
            resumed: Notify::new(),
        });
        let waiting = kernel
            .park_human_wait(&claim, ToolLaneParkingAuthority::new(lane))
            .await
            .unwrap();
        store.fail_wait_resumes.store(usize::MAX, Ordering::Release);
        let waiter = tokio::spawn(waiting.resume(CancellationToken::new()));
        while store.wait_resume_attempts.load(Ordering::Acquire) == 0 {
            tokio::task::yield_now().await;
        }
        if abort_waiter {
            waiter.abort();
            assert!(waiter.await.unwrap_err().is_cancelled());
        } else {
            assert!(
                matches!(waiter.await.unwrap(), Err(TurnError::Flush(message)) if message.contains("owned cleanup continues"))
            );
        }
        kernel.release(&claim).unwrap();
        let next = tokio::spawn({
            let kernel = kernel.clone();
            async move { kernel.claim("human-worker", CancellationToken::new()).await }
        });
        for _ in 0..10 {
            tokio::task::yield_now().await;
        }
        assert!(
            !next.is_finished(),
            "the parked activation escaped its retained claim"
        );
        store.fail_wait_resumes.store(0, Ordering::Release);
        let reclaimed = tokio::time::timeout(std::time::Duration::from_secs(6), next)
            .await
            .unwrap()
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(reclaimed.turn_id(), claim.turn_id());
        let controls = memory
            .read_controls(claim.session_id(), 0, 256)
            .await
            .unwrap();
        assert!(controls.records.iter().any(|record| matches!(
            record.body(),
            AgentControlRecordBody::WaitResumed {
                cause: WaitResumeCause::Cancel,
                ..
            }
        )));
        kernel
            .finish_turn(&reclaimed, &TurnOutcome::Completed)
            .await
            .unwrap();
        kernel.shutdown(workers).await.unwrap();
    }
}

#[tokio::test]
async fn human_wait_parks_durably_excludes_elapsed_and_reacquires_before_resuming() {
    let store = Arc::new(MemoryStore::new());
    let clock = Arc::new(HumanClock(AtomicU64::new(100)));
    let kernel = AgentKernel::recover_with_clock(store.clone(), composition(), clock.clone())
        .await
        .unwrap();
    let worker = kernel.start_workers();
    let session = SessionId::new("human-clock").unwrap();
    kernel
        .submit_message(SubmitMessage {
            session: fresh(header(session.as_str())),
            message: mailbox_message("human"),
            delivery: rsi_agent_session_protocol::MessageDelivery::NextTurn,
        })
        .await
        .unwrap();
    let _lease = kernel.register("human-worker".into()).unwrap();
    let claim = kernel
        .claim("human-worker", CancellationToken::new())
        .await
        .unwrap()
        .unwrap();
    let pool = Arc::new(Semaphore::new(1));
    let lane = Arc::new(Lane {
        pool: pool.clone(),
        permit: Mutex::new(Some(pool.clone().acquire_owned().await.unwrap())),
        resumed: Notify::new(),
    });
    let elapsed = kernel.elapsed_budget(&claim).unwrap();
    clock.0.store(110, Ordering::SeqCst);
    let waiting = kernel
        .park_human_wait(&claim, ToolLaneParkingAuthority::new(lane.clone()))
        .await
        .unwrap();
    assert_eq!(
        store
            .inspect_session(&session)
            .await
            .unwrap()
            .activation_phase,
        Some(StoreActivationPhase::Parked)
    );
    assert_eq!(pool.available_permits(), 1);
    clock.0.store(86_400_110, Ordering::SeqCst);
    assert_eq!(elapsed.consumed_ms(), 10);
    let competing = pool.clone().acquire_owned().await.unwrap();
    let resumed = tokio::spawn(waiting.resume(CancellationToken::new()));
    tokio::task::yield_now().await;
    assert!(!resumed.is_finished());
    assert_eq!(elapsed.consumed_ms(), 10);
    assert_eq!(
        store
            .inspect_session(&session)
            .await
            .unwrap()
            .activation_phase,
        Some(StoreActivationPhase::Parked)
    );
    drop(competing);
    resumed.await.unwrap().unwrap();
    assert_eq!(pool.available_permits(), 0);
    assert_eq!(
        store
            .inspect_session(&session)
            .await
            .unwrap()
            .activation_phase,
        Some(StoreActivationPhase::Running)
    );
    clock.0.fetch_add(15, Ordering::SeqCst);
    assert_eq!(elapsed.consumed_ms(), 25);
    kernel
        .finish_turn(&claim, &TurnOutcome::Completed)
        .await
        .unwrap();
    kernel.shutdown(worker).await.unwrap();
}

#[tokio::test]
async fn cancellation_and_dropped_waiter_cleanup_release_the_mutation_gate() {
    for dropped in [false, true] {
        let store = Arc::new(MemoryStore::new());
        let kernel = kernel(store.clone()).await;
        let worker = kernel.start_workers();
        kernel
            .submit_message(SubmitMessage {
                session: fresh(header("human-cancel")),
                message: mailbox_message("human"),
                delivery: rsi_agent_session_protocol::MessageDelivery::NextTurn,
            })
            .await
            .unwrap();
        let _lease = kernel.register("human-worker".into()).unwrap();
        let claim = kernel
            .claim("human-worker", CancellationToken::new())
            .await
            .unwrap()
            .unwrap();
        let pool = Arc::new(Semaphore::new(1));
        let lane = Arc::new(Lane {
            pool: pool.clone(),
            permit: Mutex::new(Some(pool.clone().acquire_owned().await.unwrap())),
            resumed: Notify::new(),
        });
        let waiting = kernel
            .park_human_wait(&claim, ToolLaneParkingAuthority::new(lane.clone()))
            .await
            .unwrap();
        let competing = pool.clone().acquire_owned().await.unwrap();
        if dropped {
            drop(waiting);
        } else {
            let cancellation = CancellationToken::new();
            let resumed = tokio::spawn(waiting.resume(cancellation.clone()));
            cancellation.cancel();
            assert!(matches!(resumed.await.unwrap(), Err(TurnError::Cancelled)));
        }
        tokio::time::timeout(std::time::Duration::from_secs(1), lane.resumed.notified())
            .await
            .unwrap();
        drop(competing);
        kernel
            .cancel_target(
                claim.session_id(),
                CancelTarget::Turn(claim.turn_id().clone()),
                None,
            )
            .await
            .unwrap();
        tokio::time::timeout(
            std::time::Duration::from_secs(1),
            kernel.finish_turn(&claim, &TurnOutcome::Cancelled),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(pool.available_permits(), 1);
        kernel.shutdown(worker).await.unwrap();
    }
}

#[tokio::test]
async fn human_wait_releases_tree_admission_and_cancelled_resume_does_not_retain_it() {
    let store = Arc::new(MemoryStore::new());
    let kernel = kernel(store.clone()).await;
    let worker = kernel.start_workers();
    let (parent, _child, _lease) = super::agent_lifecycle::active_parent_and_child(&kernel).await;
    let pool = Arc::new(Semaphore::new(1));
    let lane = Arc::new(Lane {
        pool: pool.clone(),
        permit: Mutex::new(Some(pool.clone().acquire_owned().await.unwrap())),
        resumed: Notify::new(),
    });
    let waiting = kernel
        .park_human_wait(&parent, ToolLaneParkingAuthority::new(lane))
        .await
        .unwrap();
    let mut extras = Vec::new();
    for index in 0..2 {
        kernel
            .spawn_agent(SpawnAgentRequest {
                cancellation: CancellationToken::new(),
                caller: kernel.agent_caller(&parent).unwrap(),
                child_session_id: SessionId::new(format!("human-extra-{index}")).unwrap(),
                task_name: format!("extra-{index}"),
                message_id: MessageId::new(format!("human-extra-{index}")).unwrap(),
                message: "use freed tree admission".into(),
                fork_turns: ForkTurnSelection::None,
            })
            .await
            .unwrap();
        extras.push(
            tokio::time::timeout(
                std::time::Duration::from_secs(2),
                kernel.claim("executor-review", CancellationToken::new()),
            )
            .await
            .unwrap()
            .unwrap()
            .unwrap(),
        );
    }
    let cancellation = CancellationToken::new();
    let mut resume = tokio::spawn(waiting.resume(cancellation.clone()));
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(10), &mut resume)
            .await
            .is_err()
    );
    assert_eq!(
        store
            .inspect_session(parent.session_id())
            .await
            .unwrap()
            .activation_phase,
        Some(StoreActivationPhase::Parked)
    );
    assert_eq!(pool.available_permits(), 1);
    cancellation.cancel();
    assert!(matches!(resume.await.unwrap(), Err(TurnError::Cancelled)));
    assert_eq!(pool.available_permits(), 1);
    for extra in extras {
        kernel
            .finish_turn(&extra, &TurnOutcome::Completed)
            .await
            .unwrap();
    }
    kernel.shutdown(worker).await.unwrap();
}

#[tokio::test]
async fn retired_and_shutdown_human_waits_finish_the_durable_park() {
    for mode in ["release", "withdraw", "shutdown", "outside-runtime"] {
        let store = Arc::new(MemoryStore::new());
        let kernel = kernel(store.clone()).await;
        let workers = kernel.start_workers();
        kernel
            .submit_message(SubmitMessage {
                session: fresh(header("retired-human")),
                message: mailbox_message("human"),
                delivery: rsi_agent_session_protocol::MessageDelivery::NextTurn,
            })
            .await
            .unwrap();
        let registration = kernel.register("human-worker".into()).unwrap();
        let claim = kernel
            .claim("human-worker", CancellationToken::new())
            .await
            .unwrap()
            .unwrap();
        let pool = Arc::new(Semaphore::new(1));
        let lane = Arc::new(Lane {
            pool: pool.clone(),
            permit: Mutex::new(Some(pool.clone().acquire_owned().await.unwrap())),
            resumed: Notify::new(),
        });
        let waiting = kernel
            .park_human_wait(&claim, ToolLaneParkingAuthority::new(lane.clone()))
            .await
            .unwrap();
        if mode == "shutdown" {
            kernel.shutdown(workers).await.unwrap();
            drop(waiting);
        } else {
            match mode {
                "release" => kernel.release(&claim).unwrap(),
                "withdraw" => drop(registration),
                "outside-runtime" => (),
                _ => unreachable!(),
            }
            if mode == "outside-runtime" {
                std::thread::spawn(move || drop(waiting)).join().unwrap();
            } else {
                assert!(waiting.resume(CancellationToken::new()).await.is_err());
            }
            kernel.shutdown(workers).await.unwrap();
        }
        assert_eq!(
            store
                .inspect_session(claim.session_id())
                .await
                .unwrap()
                .activation_phase,
            Some(StoreActivationPhase::Running),
            "{mode}"
        );
        assert_eq!(pool.available_permits(), 1, "{mode}");
    }
}
