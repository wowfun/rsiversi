use super::*;
use crate::{AgentKernel, tests::ClaimFixture};

async fn fixture() -> (AgentKernel, SessionId) {
    use rsi_agent_session_protocol::*;
    let store = Arc::new(rsi_agent_testkit::MemoryStore::new());
    let kernel = AgentKernel::recover(store.clone(), Arc::new(ClaimFixture))
        .await
        .unwrap();
    let id = SessionId::new("admission").unwrap();
    let header = SessionHeader::new_local(
        id.clone(),
        1,
        "/workspace",
        AgentPresetId::new("test").unwrap(),
        FrozenAgentSettings::new(
            "test",
            "system",
            rsi_ai_protocol::ModelRef::new("test", "test").unwrap(),
            rsi_sandbox::SandboxMode::ReadOnly,
            false,
        )
        .unwrap(),
    )
    .unwrap();
    let fact = SessionFact::new(
        1,
        1,
        SessionFactBody::TurnAccepted {
            turn_id: TurnId::new("turn").unwrap(),
            text: "hello".into(),
            model: None,
            reasoning_effort: None,
            sandbox: rsi_sandbox::SandboxMode::ReadOnly,
            require_approval: false,
        },
    )
    .unwrap();
    store
        .append(rsi_agent_store_protocol::AppendBatch {
            session_id: id.clone(),
            expected_seq: 0,
            header: Some(header.clone()),
            facts: vec![Arc::new(fact.clone())],
        })
        .await
        .unwrap();
    store
        .write_context_checkpoint(rsi_agent_store_protocol::WriteContextCheckpoint {
            session_id: id.clone(),
            expected_durable_seq: 1,
            checkpoint: rsi_agent_store_protocol::StoredContextCheckpoint {
                header_fingerprint: header.fingerprint().unwrap(),
                through_seq: 1,
                fact_prefix_sha256: fact_prefix_sha256([&fact]).unwrap(),
                bytes: rsi_api_protocol::ByteBudget::default()
                    .copy(b"checkpoint")
                    .unwrap(),
            },
        })
        .await
        .unwrap();
    (kernel, id)
}

#[tokio::test]
async fn large_record_fallback_releases_small_reservation_and_rejoins_fifo() {
    use futures_util::FutureExt;
    let (kernel, id) = fixture().await;
    let capacity = kernel.inner.store_read_admission.available_permits();
    let held = kernel
        .inner
        .store_read_admission
        .clone()
        .acquire_many_owned(u32::try_from(capacity - 1024 * 1024).unwrap())
        .await
        .unwrap();
    let entered = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let worker = tokio::spawn({
        let inner = kernel.inner.clone();
        let id = id.clone();
        let entered = entered.clone();
        let release = release.clone();
        async move {
            page(&inner, &id, 8, move |_, _, limit, bytes| {
                let entered = entered.clone();
                let release = release.clone();
                async move {
                    if bytes == 1024 * 1024 {
                        assert_eq!(limit, 8);
                        entered.notify_one();
                        release.notified().await;
                        Err(StoreError::ReadRequiresCapacity {
                            minimum_bytes: 2 * 1024 * 1024,
                        })
                    } else {
                        assert_eq!(limit, 1);
                        assert_eq!(bytes, 2 * INITIAL_PAGE_BYTES);
                        Ok(())
                    }
                }
            })
            .await
        }
    });
    entered.notified().await;
    let mut small = Box::pin(read(&kernel.inner, &id, 1024 * 1024, false, |_, _| async {
        Ok(())
    }));
    assert!(
        small.as_mut().now_or_never().is_none(),
        "queued before the fallback"
    );
    release.notify_one();
    let small = tokio::time::timeout(std::time::Duration::from_secs(5), small)
        .await
        .unwrap()
        .unwrap();
    assert!(
        !worker.is_finished(),
        "large fallback waits while the preceding reader owns admission"
    );
    drop(held);
    let large = tokio::time::timeout(std::time::Duration::from_secs(5), worker)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(
        kernel.inner.store_read_admission.available_permits(),
        capacity - 3 * INITIAL_PAGE_BYTES
    );
    assert_eq!(large.0.1, 1);
    drop((small, large));
    assert_eq!(
        kernel.inner.store_read_admission.available_permits(),
        capacity
    );
}

#[tokio::test]
async fn invalid_capacity_responses_are_corruption_without_further_retry() {
    for sizes in [
        [1, 1],
        [MAXIMUM_SESSION_FACT_BYTES + 1, 1],
        [INITIAL_PAGE_BYTES + 1, INITIAL_PAGE_BYTES + 2],
    ] {
        let (kernel, id) = fixture().await;
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let observed = calls.clone();
        let result = page(&kernel.inner, &id, 8, move |_, _, _, _| {
            let attempt = observed.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            async move {
                Err::<(), _>(StoreError::ReadRequiresCapacity {
                    minimum_bytes: sizes[attempt],
                })
            }
        })
        .await;
        assert!(matches!(result, Err(StoreError::Corrupt(_))));
        assert_eq!(
            calls.load(std::sync::atomic::Ordering::SeqCst),
            if sizes[0] == INITIAL_PAGE_BYTES + 1 {
                2
            } else {
                1
            }
        );
    }
}

#[tokio::test]
async fn checkpoint_global_pressure_is_a_miss_but_local_pressure_is_an_error() {
    use rsi_agent_turn_protocol::{TurnError, TurnExecution};
    let (kernel, id) = fixture().await;
    let capacity = kernel.inner.store_read_admission.available_permits();
    let held = kernel
        .inner
        .store_read_admission
        .clone()
        .acquire_many_owned(u32::try_from(capacity).unwrap())
        .await
        .unwrap();
    assert!(
        kernel
            .read_context_checkpoint(&id, rsi_api_protocol::ByteBudget::default().into())
            .await
            .unwrap()
            .is_none()
    );
    drop(held);
    assert!(matches!(
        kernel
            .read_context_checkpoint(&id, rsi_api_protocol::ByteBudget::new(1).unwrap().into())
            .await,
        Err(TurnError::Capacity)
    ));
    assert_eq!(
        kernel.inner.store_read_admission.available_permits(),
        capacity
    );
    let checkpoint = kernel
        .read_context_checkpoint(&id, rsi_api_protocol::ByteBudget::default().into())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        kernel.inner.store_read_admission.available_permits(),
        capacity
    );
    drop(checkpoint);
    assert_eq!(
        kernel.inner.store_read_admission.available_permits(),
        capacity
    );
}

#[tokio::test]
async fn cancelled_materialization_retains_bytes_until_its_worker_finishes() {
    let kernel = AgentKernel::recover(
        Arc::new(rsi_agent_testkit::MemoryStore::new()),
        Arc::new(ClaimFixture),
    )
    .await
    .unwrap();
    let capacity = kernel.inner.store_read_admission.available_permits();
    let entered = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let waiting = tokio::spawn({
        let inner = kernel.inner.clone();
        let entered = entered.clone();
        let release = release.clone();
        async move {
            read(
                &inner,
                &SessionId::new("unused").unwrap(),
                1024 * 1024,
                false,
                move |_, _| async move {
                    entered.notify_one();
                    release.notified().await;
                    Ok(())
                },
            )
            .await
        }
    });
    entered.notified().await;
    waiting.abort();
    assert!(waiting.await.unwrap_err().is_cancelled());
    assert_eq!(
        kernel.inner.store_read_admission.available_permits(),
        capacity - 1024 * 1024
    );
    release.notify_one();
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while kernel.inner.store_read_admission.available_permits() != capacity {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn closed_checkpoint_admission_is_a_store_failure() {
    use rsi_agent_turn_protocol::{TurnError, TurnExecution};
    let (kernel, id) = fixture().await;
    kernel.inner.store_read_admission.close();
    assert!(matches!(
        kernel
            .read_context_checkpoint(&id, rsi_api_protocol::ByteBudget::default().into())
            .await,
        Err(TurnError::Store(_))
    ));
}
