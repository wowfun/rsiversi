use super::*;
use futures_util::{FutureExt as _, StreamExt as _};
use rsi_session_protocol::{DraftRelease, SessionDraftControl as _};
use std::time::Duration;

#[tokio::test(start_paused = true)]
async fn directory_watch_observes_device_drafts_and_external_publication() {
    use rsi_api_protocol::{AuthenticatedDevice, CallOrigin, DeviceId};
    use rsi_session_protocol::SessionIngress as _;
    let fixture = Fixture::new().await;
    let device = AuthenticatedDevice {
        id: DeviceId::from_bytes([7; 16]),
        revoked: CancellationToken::new(),
    };
    let mut changes = fixture.service.watch_directory(&device).unwrap().unwrap();
    let request = Fixture::request("observed-draft");
    let _draft = fixture
        .service
        .create_from(request.clone(), CallOrigin::Device(device.clone()))
        .await
        .unwrap();
    assert!(matches!(changes.next().now_or_never(), Some(Some(Ok(())))));
    assert_eq!(
        fixture.service.device_drafts(&device).await.unwrap()[0].session_id,
        request.session_id
    );
    assert_eq!(
        fixture.service.release_draft(&request.session_id),
        DraftRelease::Released
    );
    assert!(matches!(changes.next().now_or_never(), Some(Some(Ok(())))));
    assert!(
        fixture
            .service
            .device_drafts(&device)
            .await
            .unwrap()
            .is_empty()
    );
    fixture
        .service
        .create_from(
            Fixture::request("expiring-device-draft"),
            CallOrigin::Device(device.clone()),
        )
        .await
        .unwrap();
    assert!(matches!(changes.next().now_or_never(), Some(Some(Ok(())))));
    tokio::time::advance(Duration::from_mins(61)).await;
    changes.next().await.unwrap().unwrap();
    assert!(
        fixture
            .service
            .device_drafts(&device)
            .await
            .unwrap()
            .is_empty()
    );
    let external = fixture.create("outside-this-device").await;
    assert!(changes.next().now_or_never().is_none());
    submit_text(external, "outside-input", "from Headless")
        .await
        .unwrap();
    assert!(matches!(changes.next().now_or_never(), Some(Some(Ok(())))));
    let scoped = fixture.service.scoped(CallOrigin::Device(device.clone()));
    let rows = scoped.list_recent(None, 10).await.unwrap();
    assert_eq!(
        rows.sessions[0].header.session_id().as_str(),
        "outside-this-device"
    );
    device.revoked.cancel();
    assert!(changes.next().await.is_none());
    fixture.stop().await;
}

#[tokio::test(start_paused = true)]
async fn directory_watch_rechecks_permissions_without_a_session_write() {
    use rsi_api_protocol::{AuthenticatedDevice, CallOrigin, DeviceId};
    use rsi_session_protocol::{SessionIngress as _, SessionProtection};
    #[derive(Debug, Default)]
    struct Policy(std::sync::Mutex<Arc<()>>);
    impl SessionProtection for Policy {
        fn catalog_generation(&self) -> Option<Arc<dyn Send + Sync>> {
            Some(self.0.lock().unwrap().clone())
        }
        fn view(
            &self,
            _: &rsi_agent_session_protocol::SessionProtectionScope,
            _: &CallOrigin,
        ) -> rsi_session_protocol::Result<CancellationToken> {
            Ok(CancellationToken::new())
        }
    }
    let fixture = Fixture::new().await;
    let device = AuthenticatedDevice {
        id: DeviceId::from_bytes([9; 16]),
        revoked: CancellationToken::new(),
    };
    let policy = Arc::new(Policy::default());
    let execution = Arc::new(crate::access::Access::default());
    let service = (*fixture.service)
        .clone()
        .with_protection(policy.clone())
        .with_execution(execution.clone());
    let mut changes = service.watch_directory(&device).unwrap().unwrap();
    assert!(changes.next().now_or_never().is_none());
    tokio::time::advance(Duration::from_secs(2)).await;
    assert!(changes.next().now_or_never().is_none());
    *policy.0.lock().unwrap() = Arc::new(());
    tokio::time::advance(Duration::from_secs(1)).await;
    assert!(matches!(changes.next().now_or_never(), Some(Some(Ok(())))));
    execution.granted.store(true, Ordering::SeqCst);
    tokio::time::advance(Duration::from_secs(1)).await;
    assert!(matches!(changes.next().now_or_never(), Some(Some(Ok(())))));
    drop(changes);
    fixture.stop().await;
}

#[tokio::test]
async fn session_stop_clears_a_draft_once_without_publication_or_command_receipt_loss() {
    use rsi_agent_turn_protocol::CancelTarget;
    for target in [
        CancelTarget::Session { keep_inbox: false },
        CancelTarget::Inbox,
    ] {
        let fixture = Fixture::new().await;
        fixture.callback.hold_inputs.store(true, Ordering::Release);
        let handle = fixture.create("stop-draft").await;
        let mut invocations = Vec::new();
        for id in ["one", "two", "three"] {
            let invocation = fixture.invoke(&handle, id, true).await;
            fixture.callback.release.add_permits(1);
            let receipt = handle.execute_command(invocation.clone()).await.unwrap();
            invocations.push((invocation, receipt));
        }
        let before = handle.draft_snapshot().await.unwrap();
        handle.wait_idle().await.unwrap();
        assert_eq!(before.command_inputs.len(), 3);
        let mut observer = handle.observe_draft().await.unwrap();
        assert_eq!(
            observer.next().await.unwrap().unwrap(),
            Some(before.revision)
        );
        assert!(
            !handle
                .cancel(CancelTarget::Session { keep_inbox: true }, None)
                .await
                .unwrap()
                .accepted
        );
        assert!(observer.next().now_or_never().is_none());
        assert!(handle.cancel(target.clone(), None).await.unwrap().accepted);
        assert_eq!(
            observer.next().await.unwrap().unwrap(),
            Some(before.revision + 1)
        );
        let after = handle.draft_snapshot().await.unwrap();
        assert!(after.command_inputs.is_empty());
        assert_eq!(after.header, before.header);
        assert!(!handle.cancel(target, None).await.unwrap().accepted);
        for (invocation, receipt) in invocations {
            assert_eq!(handle.execute_command(invocation).await.unwrap(), receipt);
        }
        assert_eq!(fixture.callback.calls.load(Ordering::Acquire), 3);
        assert!(
            handle
                .draft_snapshot()
                .await
                .unwrap()
                .command_inputs
                .is_empty()
        );
        assert!(observer.next().now_or_never().is_none());
        assert!(matches!(
            fixture.store.header(after.header.session_id()).await,
            Err(rsi_agent_store_protocol::StoreError::NotFound(_))
        ));
        fixture.stop().await;
    }
}

#[tokio::test(start_paused = true)]
async fn draft_owners_outlive_idle_deadlines_and_publication_ends_their_effect() {
    let fixture = Fixture::new().await;
    let handle = fixture.create("owned-draft").await;
    let first = handle.retain_draft().unwrap().unwrap();
    let second = handle.retain_draft().unwrap().unwrap();
    tokio::time::advance(Duration::from_mins(61)).await;
    assert!(first.is_active());
    drop(first);
    tokio::time::advance(Duration::from_mins(61)).await;
    let invocation = fixture.invoke(&handle, "late-command", true).await;
    fixture.callback.release.add_permits(1);
    handle.execute_command(invocation).await.unwrap();
    submit_text(handle.clone(), "late-publish", "publish")
        .await
        .unwrap();
    assert!(!second.is_active());
    assert!(handle.retain_draft().unwrap().is_none());
    drop(second);
    fixture.stop().await;
}

#[tokio::test(start_paused = true)]
async fn dropping_the_last_draft_owner_restores_idle_reclamation() {
    let fixture = Fixture::new().await;
    let handle = fixture.create("released-owner").await;
    let owner = handle.retain_draft().unwrap().unwrap();
    tokio::time::advance(Duration::from_mins(61)).await;
    drop(owner);
    tokio::time::advance(Duration::from_mins(59)).await;
    let mut changes = handle.observe_draft().await.unwrap();
    assert_eq!(changes.next().await.unwrap().unwrap(), Some(0));
    tokio::time::advance(Duration::from_mins(2)).await;
    assert!(matches!(
        changes.next().await,
        Some(Err(SessionError::NotFound(_)))
    ));
    assert!(matches!(
        handle.retain_draft(),
        Err(SessionError::NotFound(_))
    ));
    fixture.stop().await;
}

#[tokio::test(start_paused = true)]
async fn explicit_retirement_overrides_owners_and_stale_owners_cannot_touch_replacement() {
    let fixture = Fixture::new().await;
    let handle = fixture.create("replaced-owner").await;
    let old = handle.retain_draft().unwrap().unwrap();
    assert_eq!(
        fixture
            .service
            .release_draft(&SessionId::new("replaced-owner").unwrap()),
        DraftRelease::Released
    );
    assert!(!old.is_active());
    let replacement = fixture.create("replaced-owner").await;
    let current = replacement.retain_draft().unwrap().unwrap();
    drop(old);
    tokio::time::advance(Duration::from_mins(61)).await;
    assert!(current.is_active());
    assert_eq!(replacement.draft_snapshot().await.unwrap().revision, 0);
    fixture.stop().await;
    assert!(!current.is_active());
}

#[tokio::test]
async fn draft_notifications_coalesce_without_projection_callbacks_and_end_at_publication() {
    let fixture = Fixture::new().await;
    let handle = fixture.create("draft-notification").await;
    let gate = Arc::new(projections::Gate {
        tokens: std::sync::Mutex::new(Vec::new()),
        entered: Semaphore::new(0),
        release: Semaphore::new(0),
    });
    *fixture.callback.view_gate.lock().unwrap() = Some(gate.clone());
    let mut changes = handle.observe_draft().await.unwrap();
    assert_eq!(changes.next().await.unwrap().unwrap(), Some(0));
    assert!(changes.next().now_or_never().is_none());
    for (id, value) in [("first", true), ("second", false), ("third", true)] {
        let invocation = fixture.invoke(&handle, id, value).await;
        fixture.callback.release.add_permits(1);
        handle.execute_command(invocation).await.unwrap();
    }
    // An already pending poll may capture an earlier revision; catch up without loss.
    let mut revision = 0;
    while revision < 3 {
        let next = changes.next().await.unwrap().unwrap().unwrap();
        assert!(next > revision);
        revision = next;
    }
    assert!(gate.tokens.lock().unwrap().is_empty());
    let mut peer = fixture
        .service
        .attach(&SessionId::new("draft-notification").unwrap())
        .await
        .unwrap()
        .observe_draft()
        .await
        .unwrap();
    assert_eq!(peer.next().await.unwrap().unwrap(), Some(3));
    submit_text(handle.clone(), "publish-notification", "publish")
        .await
        .unwrap();
    assert_eq!(changes.next().await.unwrap().unwrap(), None);
    assert!(changes.next().await.is_none());
    assert_eq!(peer.next().await.unwrap().unwrap(), None);
    let mut durable = handle.observe_draft().await.unwrap();
    assert_eq!(durable.next().await.unwrap().unwrap(), None);
    assert!(durable.next().await.is_none());
    assert!(gate.tokens.lock().unwrap().is_empty());
    fixture.stop().await;
}

#[tokio::test(start_paused = true)]
async fn draft_observation_does_not_renew_or_pin_an_idle_draft() {
    let fixture = Fixture::new().await;
    let handle = fixture.create("idle-notification").await;
    tokio::time::advance(Duration::from_mins(50)).await;
    let mut changes = handle.observe_draft().await.unwrap();
    assert_eq!(changes.next().await.unwrap().unwrap(), Some(0));
    let pending = tokio::spawn(async move { changes.next().await });
    tokio::task::yield_now().await;
    tokio::time::advance(Duration::from_mins(11)).await;
    assert!(matches!(
        pending.await.unwrap(),
        Some(Err(SessionError::NotFound(_)))
    ));
    assert!(matches!(
        handle.draft_snapshot().await,
        Err(SessionError::NotFound(_))
    ));
    fixture.stop().await;
}

#[tokio::test]
async fn draft_observers_release_on_discard_and_shutdown() {
    let fixture = Fixture::new().await;
    let handle = fixture.create("discard-notification").await;
    let mut changes = handle.observe_draft().await.unwrap();
    assert_eq!(changes.next().await.unwrap().unwrap(), Some(0));
    assert_eq!(
        fixture
            .service
            .release_draft(&SessionId::new("discard-notification").unwrap()),
        DraftRelease::Released
    );
    assert!(matches!(
        changes.next().await,
        Some(Err(SessionError::NotFound(_)))
    ));
    assert!(changes.next().await.is_none());
    let other = fixture.create("stop-notification").await;
    let mut changes = other.observe_draft().await.unwrap();
    assert_eq!(changes.next().await.unwrap().unwrap(), Some(0));
    fixture.stop().await;
    assert!(matches!(
        changes.next().await,
        Some(Err(SessionError::ShuttingDown))
    ));
    assert!(changes.next().await.is_none());
}
