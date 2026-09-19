use super::*;
use rsi_agent_store_sqlite::SqliteStore;
use std::time::Duration;

async fn sqlite_history(store: &SqliteStore, name: &str) -> SessionId {
    let id = SessionId::new(name).unwrap();
    let turn = TurnId::new("history").unwrap();
    let terminal = SessionFact::new(
        2,
        2,
        SessionFactBody::TurnTerminal {
            turn_id: turn.clone(),
            outcome: TurnOutcome::Completed,
            result: None,
        },
    )
    .unwrap();
    rsi_agent_testkit::append_history_fixture(
        store,
        AppendBatch {
            session_id: id.clone(),
            expected_seq: 0,
            header: Some(header(name)),
            facts: vec![accepted_fact(1, &turn).into(), terminal.into()],
        },
    )
    .await
    .unwrap();
    id
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn sqlite_cold_read_leaves_warm_observation_terminal_and_new_session_progress() {
    let root = tempfile::tempdir().unwrap();
    let store = SqliteStore::open(root.path()).unwrap();
    let cold = sqlite_history(&store, "cold-a").await;
    let warm = sqlite_history(&store, "warm-b").await;
    drop(store);
    let store = Arc::new(SqliteStore::open(root.path()).unwrap());
    store.validate_session(&warm).await.unwrap();
    let kernel =
        AgentKernel::recover_with_clock(store.clone(), composition(), Arc::new(FixedClock))
            .await
            .unwrap();
    let workers = kernel.start_workers();
    let (entered, release) = store.pause_next_validation();
    let cold_read = tokio::spawn({
        let kernel = kernel.clone();
        async move { kernel.observe(&cold, 0).await }
    });
    tokio::time::timeout(Duration::from_secs(2), entered)
        .await
        .unwrap()
        .unwrap();
    let before = store.validation_counts();
    let progress = tokio::time::timeout(Duration::from_secs(5), async {
        let mut stream = kernel.observe(&warm, 0).await.unwrap();
        assert!(stream.next().await.unwrap().is_ok());
        drop(stream);
        for _ in 0..2 {
            let submitted = resume_submit(&kernel, &warm, "warm suffix").await;
            assert!(
                kernel
                    .cancel(&warm, &submitted.turn_id, None)
                    .await
                    .unwrap()
                    .accepted
            );
        }
        submit(&kernel, "new-c", "new session while A validates").await;
    })
    .await;
    // Always release the blocking worker before asserting the progress oracle.
    drop(release);
    progress.expect("warm/new sessions were serialized behind cold validation");
    assert_eq!(store.validation_counts().0, before.0);
    drop(cold_read.await.unwrap().unwrap());
    kernel.shutdown(workers).await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn blocked_flush_does_not_hold_a_round_open_for_other_sessions() {
    let memory = Arc::new(MemoryStore::new());
    let store = Arc::new(FactReadRaceStore::new(memory));
    store.pause_append_at.store(1, Ordering::Release);
    let kernel =
        AgentKernel::recover_with_clock(store.clone(), composition(), Arc::new(FixedClock))
            .await
            .unwrap();
    let workers = kernel.start_workers();
    let first = tokio::spawn({
        let kernel = kernel.clone();
        async move { submit(&kernel, "a-blocked", "A").await }
    });
    tokio::time::timeout(Duration::from_secs(2), store.append_blocked.notified())
        .await
        .unwrap();
    let progress = tokio::time::timeout(Duration::from_secs(5), async {
        submit(&kernel, "b-progress", "B first").await;
        resume_submit(&kernel, &SessionId::new("b-progress").unwrap(), "B second").await;
        for index in 0..32 {
            let id = format!("c-progress-{index}");
            submit(&kernel, &id, "C first").await;
            resume_submit(&kernel, &SessionId::new(id).unwrap(), "C second").await;
        }
    })
    .await;
    store.release_append.notify_one();
    progress.expect("one slow flush must not retain an entire scheduling round");
    first.await.unwrap();
    kernel.shutdown(workers).await.unwrap();
}

#[tokio::test]
async fn cancelled_materialization_keeps_admission_until_the_dispatched_read_finishes() {
    let memory = Arc::new(MemoryStore::new());
    append_terminal_history(&memory, "materializing", 1).await;
    append_terminal_history(&memory, "waiting", 1).await;
    let store = Arc::new(FactReadRaceStore::new(memory));
    let kernel =
        AgentKernel::recover_with_clock(store.clone(), composition(), Arc::new(FixedClock))
            .await
            .unwrap();
    store.pause_next_read();
    let first = tokio::spawn({
        let kernel = kernel.clone();
        async move {
            kernel
                .observe(&SessionId::new("materializing").unwrap(), 0)
                .await
        }
    });
    store.read_captured.notified().await;
    first.abort();
    assert!(matches!(first.await, Err(error) if error.is_cancelled()));
    let count = store.read_attempts.load(Ordering::Acquire);
    let id = SessionId::new("waiting").unwrap();
    let mut second = Box::pin(kernel.observe(&id, 0));
    assert!(futures_util::poll!(&mut second).is_pending());
    assert_eq!(
        store.read_attempts.load(Ordering::Acquire),
        count,
        "cancellation released an in-use payload reservation"
    );
    store.release_read.notify_one();
    let _stream = tokio::time::timeout(Duration::from_secs(2), second)
        .await
        .unwrap()
        .unwrap();
    assert!(store.read_attempts.load(Ordering::Acquire) > count);
}

async fn resume_submit(
    kernel: &AgentKernel,
    id: &SessionId,
    text: &str,
) -> rsi_agent_turn_protocol::SubmittedTurn {
    kernel
        .submit(SubmitTurn {
            reasoning_effort: None,
            turn_id: client_turn_id(),
            session: SubmitSession::Resume(kernel.prepare_resume(id).await.unwrap()),
            text: text.into(),
            model: None,
            sandbox: None,
        })
        .await
        .unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn sqlite_preparation_stays_pinned_during_kernel_budget_wait_and_cache_churn() {
    let root = tempfile::tempdir().unwrap();
    let store = SqliteStore::open(root.path()).unwrap();
    let first_id = sqlite_history(&store, "holds-budget").await;
    let second_id = sqlite_history(&store, "prepared-waiter").await;
    drop(store);
    let store = Arc::new(SqliteStore::open(root.path()).unwrap());
    let kernel =
        AgentKernel::recover_with_clock(store.clone(), composition(), Arc::new(FixedClock))
            .await
            .unwrap();
    let (entered, release) = store.pause_next_fact_page();
    let first = tokio::spawn({
        let kernel = kernel.clone();
        async move { kernel.observe(&first_id, 0).await }
    });
    tokio::time::timeout(Duration::from_secs(2), entered)
        .await
        .unwrap()
        .unwrap();
    let validations = store.validation_counts().0;
    let second = tokio::spawn({
        let kernel = kernel.clone();
        async move { kernel.observe(&second_id, 0).await }
    });
    tokio::time::timeout(Duration::from_secs(2), async {
        while store.validation_counts().0 == validations {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    // Wait for validation completion, then saturate the eviction cache through
    // trusted writes on the independent writer lane while the read budget is full.
    for index in 0..512 {
        sqlite_history(&store, &format!("churn-{index}")).await;
    }
    let after_churn = store.validation_counts().0;
    assert!(!second.is_finished());
    drop(release);
    drop(
        tokio::time::timeout(Duration::from_secs(2), first)
            .await
            .unwrap()
            .unwrap()
            .unwrap(),
    );
    drop(
        tokio::time::timeout(Duration::from_secs(2), second)
            .await
            .unwrap()
            .unwrap()
            .unwrap(),
    );
    assert_eq!(
        store.validation_counts().0,
        after_churn,
        "prepared waiter revalidated while holding payload admission"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn shutdown_drains_the_accepted_suffix_beyond_the_inflight_batch() {
    let memory = Arc::new(MemoryStore::new());
    let store = Arc::new(FactReadRaceStore::new(memory.clone()));
    let kernel =
        AgentKernel::recover_with_clock(store.clone(), composition(), Arc::new(FixedClock))
            .await
            .unwrap();
    let workers = kernel.start_workers();
    submit(&kernel, "drain-suffix", "drain every accepted page").await;
    let _lease = kernel.register("drain-executor".into()).unwrap();
    let claim = kernel
        .claim("drain-executor", CancellationToken::new())
        .await
        .unwrap()
        .unwrap();
    let effect = EffectId::new("drain-model").unwrap();
    let intent = kernel
        .publish(
            &claim,
            vec![
                model_intent_fact(2, claim.turn_id(), &effect)
                    .body()
                    .clone(),
            ],
        )
        .await
        .unwrap()
        .published();
    kernel.flush(&claim, intent[0].seq()).await.unwrap();
    store.pause_append_at.store(3, Ordering::Release);
    kernel
        .publish(
            &claim,
            vec![
                model_started_fact(3, claim.turn_id(), &effect)
                    .body()
                    .clone(),
                SessionFactBody::ModelEvent {
                    turn_id: claim.turn_id().clone(),
                    effect_id: effect.clone(),
                    purpose: rsi_agent_session_protocol::ModelEventPurpose::Conversation,
                    event: LanguageEvent::ContentStarted {
                        index: 0,
                        content: ContentStart::Text,
                    },
                },
            ],
        )
        .await
        .unwrap()
        .published();
    tokio::time::timeout(Duration::from_secs(2), store.append_blocked.notified())
        .await
        .unwrap();
    let mut through = 0;
    for _ in 0..2 {
        let facts = kernel
            .publish(
                &claim,
                (0..300)
                    .map(|_| SessionFactBody::ModelEvent {
                        turn_id: claim.turn_id().clone(),
                        effect_id: effect.clone(),
                        purpose: rsi_agent_session_protocol::ModelEventPurpose::Conversation,
                        event: LanguageEvent::ContentDelta {
                            index: 0,
                            delta: ContentDelta::Text("suffix".into()),
                        },
                    })
                    .collect(),
            )
            .await
            .unwrap()
            .published();
        through = facts.last().unwrap().seq();
    }
    let stopping = tokio::spawn({
        let kernel = kernel.clone();
        async move { kernel.shutdown(workers).await }
    });
    store.release_append.notify_one();
    tokio::time::timeout(Duration::from_secs(5), stopping)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(
        memory
            .read_watermarks(claim.session_id())
            .await
            .unwrap()
            .durable_fact_seq,
        through
    );
    assert!(
        store.append_attempts.load(Ordering::Acquire) >= 5,
        "suffix larger than one page was not drained"
    );
}
