mod support;
use futures_util::StreamExt as _;
use rsi_api_protocol::{ApiError, ApiOutput, ApiStream, AuthenticatedDevice, CallOrigin, DeviceId};
use rsi_ui::{ActionInput, PresentationAction};
use rsi_ui_api::{CatalogPage, CatalogRequest, ExportScope, Invoke, Item, Source};
use std::sync::atomic::Ordering;
use support::*;
use tokio_util::sync::CancellationToken;

async fn item(stream: &mut ApiStream) -> Item {
    let message = tokio::time::timeout(std::time::Duration::from_secs(5), stream.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    serde_json::from_slice(message.json.as_bytes()).unwrap()
}
fn action(application: &str, item: &Item) -> Invoke {
    Invoke {
        application: application.into(),
        action: PresentationAction {
            presentation: item.snapshot.presentation.clone(),
            revision: item.snapshot.revision,
            action: "run".into(),
        },
        ticket: item.ticket.clone().unwrap(),
        input: ActionInput::default(),
    }
}
fn device(id: u8) -> CallOrigin {
    CallOrigin::Device(AuthenticatedDevice {
        id: DeviceId::from_bytes([id; 16]),
        revoked: CancellationToken::new(),
    })
}

#[tokio::test]
async fn catalog_binds_semantic_scope_and_observe_multiplexes_real_targets() {
    let fixture = Fixture::new().await;
    let origin = device(3);
    let ApiOutput::Reply(reply) = fixture
        .call(
            "catalog",
            origin.clone(),
            &CatalogRequest {
                scope: ExportScope {
                    kind: "fixture".into(),
                    key: "allowed".into(),
                },
                after: None,
                maximum: 64,
            },
        )
        .await
        .unwrap()
    else {
        panic!("catalog reply")
    };
    let page: CatalogPage = serde_json::from_slice(reply.json.as_bytes()).unwrap();
    assert_eq!(page.entries[0].bundle, "fixture");
    assert_eq!(page.entries[0].surface, "panel");
    assert_eq!(fixture.binder.closed.load(Ordering::SeqCst), 1);
    assert!(matches!(
        fixture
            .call(
                "catalog",
                origin.clone(),
                &CatalogRequest {
                    scope: ExportScope {
                        kind: "rsi.local.context".into(),
                        key: "allowed".into()
                    },
                    after: None,
                    maximum: 64
                }
            )
            .await,
        Err(ApiError::Unauthorized)
    ));
    let ApiOutput::Stream(mut stream) = fixture
        .call("observe", origin.clone(), &observe("app", 2))
        .await
        .unwrap()
    else {
        panic!("observe")
    };
    let first = item(&mut stream).await;
    let second = item(&mut stream).await;
    assert_ne!(
        first.snapshot.presentation.reference.target,
        second.snapshot.presentation.reference.target
    );
    assert!(matches!(
        fixture
            .call("observe", origin.clone(), &observe("app", 1))
            .await,
        Err(ApiError::Capacity)
    ));
    let request = action("app", &first);
    assert!(matches!(
        fixture.call("invoke", device(4), &request).await,
        Err(ApiError::Unavailable)
    ));
    let mut forged = request;
    forged.action.presentation.reference.target = second.snapshot.presentation.reference.target;
    assert!(matches!(
        fixture.call("invoke", origin.clone(), &forged).await,
        Err(ApiError::Unavailable)
    ));
    let source = Source {
        application: "app".into(),
        presentation: first.snapshot.presentation.clone(),
        revision: first.snapshot.revision,
        name: "raw".into(),
        offset: 0,
        maximum: 3,
    };
    let ApiOutput::Reply(reply) = fixture
        .call("source", origin.clone(), &source)
        .await
        .unwrap()
    else {
        panic!("binary source")
    };
    assert_eq!(reply.binary.unwrap().as_bytes(), b"\0\xffs");
    let mut forged = source;
    forged.name = "undeclared".into();
    assert!(matches!(
        fixture.call("source", origin, &forged).await,
        Err(ApiError::Unavailable)
    ));
    assert_eq!(fixture.source.entered.load(Ordering::SeqCst), 0);
    drop(stream);
    until(|| fixture.binder.closed.load(Ordering::SeqCst) == 3).await;
    fixture.close().await;
}

#[tokio::test]
async fn dropped_action_waiter_and_observer_do_not_cancel_admitted_mutation() {
    let fixture = Fixture::new().await;
    let ApiOutput::Stream(mut stream) = fixture
        .call("observe", CallOrigin::Local, &observe("app", 1))
        .await
        .unwrap()
    else {
        panic!("observe")
    };
    let first = item(&mut stream).await;
    let request = action("app", &first);
    let waiter = fixture.call("invoke", CallOrigin::Local, &request);
    drop(waiter);
    until(|| fixture.source.entered.load(Ordering::SeqCst) == 1).await;
    assert!(matches!(
        fixture.call("invoke", CallOrigin::Local, &request).await,
        Err(ApiError::OutcomeUnknown)
    ));
    drop(stream);
    assert!(
        tokio::time::timeout(
            std::time::Duration::from_millis(50),
            until(|| fixture.binder.closed.load(Ordering::SeqCst) != 0),
        )
        .await
        .is_err()
    );
    assert_eq!(fixture.binder.closed.load(Ordering::SeqCst), 0);
    assert_eq!(fixture.source.completed.load(Ordering::SeqCst), 0);
    fixture.source.gate.add_permits(1);
    until(|| fixture.binder.closed.load(Ordering::SeqCst) == 1).await;
    assert_eq!(fixture.source.completed.load(Ordering::SeqCst), 1);
    fixture.close().await;
}

#[tokio::test]
async fn capacity_failure_consumes_ticket_before_ui_admission() {
    let fixture = Fixture::new().await;
    let ApiOutput::Stream(mut stream) = fixture
        .call("observe", CallOrigin::Local, &observe("app", 9))
        .await
        .unwrap()
    else {
        panic!("observe")
    };
    let mut shown = Vec::new();
    for _ in 0..9 {
        shown.push(item(&mut stream).await);
    }
    let mut waiters = Vec::new();
    for item in &shown[..8] {
        waiters.push(fixture.call("invoke", CallOrigin::Local, &action("app", item)));
    }
    until(|| fixture.source.entered.load(Ordering::SeqCst) == 8).await;
    let rejected = action("app", &shown[8]);
    assert!(matches!(
        fixture.call("invoke", CallOrigin::Local, &rejected).await,
        Err(ApiError::Capacity)
    ));
    assert!(matches!(
        fixture.call("invoke", CallOrigin::Local, &rejected).await,
        Err(ApiError::OutcomeUnknown)
    ));
    assert_eq!(fixture.source.entered.load(Ordering::SeqCst), 8);
    fixture.source.gate.add_permits(8);
    for waiter in waiters {
        waiter.await.unwrap();
    }
    assert_eq!(fixture.source.completed.load(Ordering::SeqCst), 8);
    drop(stream);
    fixture.close().await;
}

#[tokio::test]
async fn revocation_closes_unpolled_observer_and_failed_binding_rolls_back() {
    let fixture = Fixture::new().await;
    let origin = device(5);
    let ApiOutput::Stream(mut stream) = fixture
        .call("observe", origin.clone(), &observe("revoked", 1))
        .await
        .unwrap()
    else {
        panic!("observe")
    };
    let _shown = item(&mut stream).await;
    let CallOrigin::Device(device) = origin else {
        unreachable!()
    };
    device.revoked.cancel();
    until(|| fixture.binder.closed.load(Ordering::SeqCst) == 1).await;
    assert!(matches!(
        stream.next().await,
        Some(Err(ApiError::Unauthorized))
    ));
    let mut request = observe("partial", 2);
    request.selections[1].scope.key = "denied".into();
    let ApiOutput::Stream(mut stream) = fixture
        .call("observe", CallOrigin::Local, &request)
        .await
        .unwrap()
    else {
        panic!("observe")
    };
    assert!(matches!(
        stream.next().await,
        Some(Err(ApiError::Unauthorized))
    ));
    assert_eq!(fixture.binder.closed.load(Ordering::SeqCst), 2);
    assert_eq!(fixture.binder.bound.load(Ordering::SeqCst), 2);
    fixture.close().await;
}

#[tokio::test]
async fn transport_bytes_keep_snapshot_count_and_budget_after_observer_retirement() {
    let fixture = Fixture::new().await;
    let ApiOutput::Stream(mut stream) = fixture
        .call("observe", CallOrigin::Local, &observe("app", 1))
        .await
        .unwrap()
    else {
        panic!("observe")
    };
    let message = stream.next().await.unwrap().unwrap();
    let escaped_item = message.json.clone();
    let escaped = message.json.into_bytes().slice(1..2);
    assert_eq!(fixture.ui.presentation_usage().1, 1);
    fixture.source.invalidate();
    let next = item(&mut stream).await;
    assert_eq!(next.snapshot.revision, 2);
    assert!(next.ticket.as_ref().unwrap().starts_with("input-"));
    assert_ne!(
        next.ticket,
        serde_json::from_slice::<Item>(escaped_item.as_bytes())
            .unwrap()
            .ticket
    );
    drop(escaped_item);
    assert_eq!(fixture.ui.presentation_usage().1, 2);
    drop(stream);
    until(|| fixture.binder.closed.load(Ordering::SeqCst) == 1).await;
    let usage = fixture.ui.presentation_usage();
    assert_eq!((usage.0, usage.1), (0, 1));
    assert!(usage.2 > 0);
    drop(escaped);
    assert_eq!(fixture.ui.presentation_usage(), (0, 0, 0));
    fixture.close().await;
}

#[tokio::test]
async fn target_cleanup_failure_is_reported_even_after_observer_disconnect() {
    let fixture = Fixture::new().await;
    let ApiOutput::Stream(mut stream) = fixture
        .call("observe", CallOrigin::Local, &observe("app", 1))
        .await
        .unwrap()
    else {
        panic!("observe")
    };
    let _ = item(&mut stream).await;
    fixture.binder.fail_close.store(true, Ordering::SeqCst);
    drop(stream);
    until(|| fixture.binder.closed.load(Ordering::SeqCst) == 1).await;
    assert!(matches!(
        fixture.api.close().await,
        Err(ApiError::Backend(_))
    ));
    fixture.registry.close().await;
    assert!(fixture.runtime.shutdown().await.is_clean());
}

#[derive(Debug)]
struct ManySurfaces(std::sync::Arc<support::Source>, usize);
#[async_trait::async_trait]
impl rsi_meta::PluginFactory for ManySurfaces {
    fn prepare(&self, _: &serde_json::Value) -> rsi_meta::Result<rsi_meta::PreparedActivation> {
        Ok(rsi_meta::PreparedActivation::new(serde_json::Value::Null)
            .requiring_local::<rsi_ui::UiContract>())
    }
    async fn activate(&self, plan: rsi_meta::ActivationPlan) -> rsi_meta::Result<()> {
        let lease = plan
            .local::<rsi_ui::UiContract>()?
            .register(
                &plan,
                rsi_ui::Contributions {
                    name: format!("many-{}", self.1),
                    surfaces: (0..32)
                        .map(|index| rsi_ui::SurfaceContribution {
                            name: format!("surface-{index:03}"),
                            title: "\u{0001}".repeat(256),
                            target: rsi_ui::TargetKind::Surface,
                            renderer: self.0.clone(),
                        })
                        .collect(),
                    actions: vec![],
                    renderers: vec![],
                },
            )
            .unwrap();
        plan.defer(
            "many surfaces",
            Box::new(move || {
                Box::pin(async move {
                    lease.dispose().await;
                    Ok(())
                })
            }),
        )
    }
}

#[tokio::test]
async fn catalog_pages_bound_full_json_escaping_and_resume_by_logical_name() {
    let fixture = Fixture::new().await;
    for index in 0..4 {
        support::apply(
            &fixture.runtime.root(),
            &format!("many-{index}"),
            ManySurfaces(fixture.source.clone(), index),
        )
        .await;
    }
    let mut request = rsi_ui_api::CatalogRequest {
        scope: ExportScope {
            kind: "fixture".into(),
            key: "allowed".into(),
        },
        after: None,
        maximum: 64,
    };
    let mut names = Vec::new();
    loop {
        let ApiOutput::Reply(reply) = fixture
            .call("catalog", CallOrigin::Local, &request)
            .await
            .unwrap()
        else {
            panic!("catalog")
        };
        assert!(reply.json.len() <= rsi_ui_api::MAXIMUM_ITEM_BYTES);
        let page: rsi_ui_api::CatalogPage = serde_json::from_slice(reply.json.as_bytes()).unwrap();
        assert!(page.entries.len() <= 64);
        names.extend(
            page.entries
                .into_iter()
                .map(|entry| (entry.bundle, entry.surface)),
        );
        let Some(next) = page.next else {
            break;
        };
        request.after = Some(next);
    }
    assert_eq!(names.len(), 129);
    assert!(names.windows(2).all(|pair| pair[0] < pair[1]));
    assert_eq!(fixture.binder.closed.load(Ordering::SeqCst), 3);
    fixture.close().await;
}

#[tokio::test]
async fn refresh_during_action_waits_for_its_reply_and_preserves_ticket_fences() {
    let fixture = Fixture::new().await;
    let ApiOutput::Stream(mut stream) = fixture
        .call("observe", CallOrigin::Local, &observe("busy", 1))
        .await
        .unwrap()
    else {
        panic!("observe");
    };
    let first = item(&mut stream).await;
    let request = action("busy", &first);
    let running = fixture.call("invoke", CallOrigin::Local, &request);
    until(|| fixture.source.entered.load(Ordering::SeqCst) == 1).await;
    fixture.source.invalidate();
    assert!(
        tokio::time::timeout(
            std::time::Duration::from_millis(50),
            until(|| fixture.source.refreshes.load(Ordering::SeqCst) != 1),
        )
        .await
        .is_err()
    );
    assert_eq!(
        fixture.source.refreshes.load(Ordering::SeqCst),
        1,
        "invalidation waits for an admitted action instead of invalidating its predecessor"
    );
    assert!(matches!(
        fixture.call("invoke", CallOrigin::Local, &request).await,
        Err(ApiError::OutcomeUnknown)
    ));
    fixture.source.gate.add_permits(1);
    running.await.unwrap();
    assert_eq!(fixture.source.completed.load(Ordering::SeqCst), 1);
    let fresh = loop {
        let next = item(&mut stream).await;
        if next.ticket.is_some() {
            break next;
        }
    };
    let mut stale = action("busy", &fresh);
    stale.action.revision = first.snapshot.revision;
    assert!(matches!(
        fixture.call("invoke", CallOrigin::Local, &stale).await,
        Err(ApiError::OutcomeUnknown)
    ));
    assert!(matches!(
        fixture.call("invoke", CallOrigin::Local, &request).await,
        Err(ApiError::OutcomeUnknown)
    ));
    assert_eq!(fixture.source.entered.load(Ordering::SeqCst), 1);
    drop(stream);
    fixture.close().await;
}

#[tokio::test]
async fn diagnostic_only_refresh_keeps_the_delivered_ticket_and_sends_no_duplicate() {
    let fixture = Fixture::new().await;
    let ApiOutput::Stream(mut stream) = fixture
        .call("observe", CallOrigin::Local, &observe("diagnostic", 1))
        .await
        .unwrap()
    else {
        panic!("observe");
    };
    let first = item(&mut stream).await;
    let refreshes = fixture.source.refreshes.load(Ordering::SeqCst);
    fixture.source.fail_refresh.store(true, Ordering::SeqCst);
    fixture.source.invalidate();
    until(|| fixture.source.refreshes.load(Ordering::SeqCst) > refreshes).await;
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(50), stream.next())
            .await
            .is_err(),
        "a diagnostic must not retransmit or rotate the delivered snapshot ticket"
    );
    fixture.source.gate.add_permits(1);
    fixture
        .call("invoke", CallOrigin::Local, &action("diagnostic", &first))
        .await
        .unwrap();
    assert_eq!(fixture.source.completed.load(Ordering::SeqCst), 1);
    let next = item(&mut stream).await;
    assert_ne!(next.ticket, first.ticket);
    drop(stream);
    fixture.close().await;
}
