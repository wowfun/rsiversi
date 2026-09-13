use super::*;
use futures_util::StreamExt as _;
use rsi_agent_session_protocol::ProjectionCursor;
use rsi_session_protocol::SelectDraftPreset;
use std::time::Duration;

#[derive(Debug)]
pub(super) struct Gate {
    pub(super) tokens: std::sync::Mutex<Vec<CancellationToken>>,
    pub(super) entered: Semaphore,
    pub(super) release: Semaphore,
}

#[tokio::test]
async fn capture_capacity_precedes_callbacks_and_drop_retirement_cancel_all_active_children() {
    let fixture = Fixture::new().await;
    let handle = fixture.create("bounded-projection").await;
    let gate = Arc::new(Gate {
        tokens: std::sync::Mutex::new(Vec::new()),
        entered: Semaphore::new(0),
        release: Semaphore::new(0),
    });
    *fixture.callback.view_gate.lock().unwrap() = Some(gate.clone());
    let mut tasks = Vec::new();
    for _ in 0..9 {
        tasks.push(tokio::spawn({
            let handle = handle.clone();
            async move { handle.observe_projections().await }
        }));
    }
    gate.entered.acquire_many(9).await.unwrap().forget();
    assert_eq!(fixture.kernel.observer_snapshot().projection.current, 9);
    assert!(matches!(
        handle.observe_projections().await,
        Err(SessionError::Capacity)
    ));
    let task = tasks.pop().unwrap();
    task.abort();
    assert!(task.await.is_err());
    assert_eq!(
        gate.tokens
            .lock()
            .unwrap()
            .iter()
            .filter(|token| token.is_cancelled())
            .count(),
        1
    );
    *fixture.callback.view_gate.lock().unwrap() = None;
    let mut next = handle.observe_projections().await.unwrap();
    next.next().await.unwrap().unwrap();
    fixture.service.stop().await;
    for task in tasks {
        assert!(matches!(
            task.await.unwrap(),
            Err(SessionError::ShuttingDown)
        ));
    }
    assert!(next.next().await.is_none());
    assert_eq!(fixture.kernel.observer_snapshot().total.current, 0);
    assert!(
        gate.tokens
            .lock()
            .unwrap()
            .iter()
            .all(CancellationToken::is_cancelled)
    );
    fixture.stop().await;
}

#[tokio::test]
async fn draft_projection_reconnect_coalesces_changes_and_follows_actual_publication() {
    let fixture = Fixture::new().await;
    let handle = fixture.create("projected-draft").await;
    let mut stream = handle.observe_projections().await.unwrap();
    let initial = stream.next().await.unwrap().unwrap();
    assert_eq!(
        initial.snapshot().cursor(),
        ProjectionCursor::Draft { revision: 0 }
    );
    assert_eq!(
        initial.snapshot().entries()[0].view().unwrap().value(),
        &false
    );
    for (id, value) in [("first", true), ("second", false), ("third", true)] {
        let invocation = fixture.invoke(&handle, id, value).await;
        fixture.callback.release.add_permits(1);
        handle.execute_command(invocation).await.unwrap();
    }
    let changed = stream.next().await.unwrap().unwrap();
    assert_eq!(
        changed.snapshot().cursor(),
        ProjectionCursor::Draft { revision: 3 }
    );
    assert_eq!(
        changed.snapshot().entries()[0].view().unwrap().value(),
        &true
    );
    let mut reconnected = fixture
        .service
        .attach(&SessionId::new("projected-draft").unwrap())
        .await
        .unwrap()
        .observe_projections()
        .await
        .unwrap();
    let reconnect = reconnected.next().await.unwrap().unwrap();
    assert_eq!(reconnect.snapshot(), changed.snapshot());
    submit_text(handle.clone(), "publish-projection", "publish")
        .await
        .unwrap();
    let durable = stream.next().await.unwrap().unwrap();
    assert!(
        matches!(durable.snapshot().cursor(), ProjectionCursor::Durable { fact_seq: 0, control_seq } if control_seq > 0)
    );
    assert!(
        durable
            .snapshot()
            .cursor()
            .can_follow(changed.snapshot().cursor())
    );
    assert_eq!(
        durable.snapshot().entries()[0].view().unwrap().value(),
        &true
    );
    let invocation = fixture.invoke(&handle, "durable-off", false).await;
    fixture.callback.release.add_permits(1);
    handle.execute_command(invocation).await.unwrap();
    let idle = stream.next().await.unwrap().unwrap();
    assert!(
        matches!(idle.snapshot().cursor(), ProjectionCursor::Durable { fact_seq: 0, control_seq } if control_seq > 0)
    );
    assert_ne!(idle.snapshot().cursor(), durable.snapshot().cursor());
    assert_eq!(idle.snapshot().entries()[0].view().unwrap().value(), &false);
    assert_eq!(
        initial.snapshot().entries()[0].view().unwrap().value(),
        &false
    );
    drop(stream);
    drop(reconnected);
    fixture.stop().await;
}

#[tokio::test]
async fn same_preset_resets_values_and_another_preset_ends_the_old_header_subscription() {
    let fixture = Fixture::new().await;
    let handle = fixture.create("preset-projection").await;
    let invocation = fixture.invoke(&handle, "on", true).await;
    fixture.callback.release.add_permits(1);
    handle.execute_command(invocation).await.unwrap();
    let mut stream = handle.observe_projections().await.unwrap();
    assert_eq!(
        stream.next().await.unwrap().unwrap().snapshot().entries()[0]
            .view()
            .unwrap()
            .value(),
        &true
    );
    handle
        .select_preset(SelectDraftPreset {
            preset_id: AgentPresetId::new("fixture").unwrap(),
            expected_revision: 1,
        })
        .await
        .unwrap();
    let reset = stream.next().await.unwrap().unwrap();
    assert_eq!(
        reset.snapshot().cursor(),
        ProjectionCursor::Draft { revision: 2 }
    );
    assert_eq!(
        reset.snapshot().entries()[0].view().unwrap().value(),
        &false
    );
    handle
        .select_preset(SelectDraftPreset {
            preset_id: AgentPresetId::new("other").unwrap(),
            expected_revision: 2,
        })
        .await
        .unwrap();
    assert!(stream.next().await.is_none());
    let mut fresh = handle.observe_projections().await.unwrap();
    let rebound = fresh.next().await.unwrap().unwrap();
    assert_ne!(
        rebound.snapshot().header_sha256(),
        reset.snapshot().header_sha256()
    );
    assert_eq!(
        rebound.snapshot().cursor(),
        ProjectionCursor::Draft { revision: 3 }
    );
    drop(fresh);
    fixture.stop().await;
}

#[tokio::test(start_paused = true)]
async fn idle_projection_does_not_keep_a_draft_lease_alive_or_revive_it_after_expiry() {
    let fixture = Fixture::new().await;
    let handle = fixture.create("expiring-projection").await;
    let mut stream = handle.observe_projections().await.unwrap();
    let retained = stream.next().await.unwrap().unwrap();
    tokio::time::advance(Duration::from_secs(3661)).await;
    tokio::task::yield_now().await;
    assert!(matches!(
        stream.next().await.unwrap(),
        Err(SessionError::NotFound(_))
    ));
    assert!(handle.observe_projections().await.is_err());
    assert_eq!(
        retained.snapshot().cursor(),
        ProjectionCursor::Draft { revision: 0 }
    );
    fixture.stop().await;
}

#[tokio::test]
async fn retiring_the_service_ends_idle_projection_subscriptions() {
    let fixture = Fixture::new().await;
    let handle = fixture.create("retiring-projection").await;
    let mut stream = handle.observe_projections().await.unwrap();
    stream.next().await.unwrap().unwrap();
    fixture.service.stop().await;
    assert!(stream.next().await.is_none());
    assert!(matches!(
        handle.observe_projections().await,
        Err(SessionError::ShuttingDown)
    ));
    fixture.stop().await;
}
