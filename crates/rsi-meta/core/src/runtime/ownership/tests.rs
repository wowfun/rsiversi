use super::OnceClaim;
use super::test_support::{ContextFactory, CountHandler};
use crate::{EventOptions, FactoryIdentity, FiberState, Runtime};
use futures_util::FutureExt as _;
use serde_json::Value;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use tokio::sync::Notify;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn retirement_claim_between_once_removal_and_effect_claim_skips_without_hanging() {
    let runtime = Runtime::default();
    let captured = Arc::new(Mutex::new(None));
    let fiber = runtime
        .root()
        .apply(
            Arc::new(ContextFactory {
                identity: FactoryIdentity::builtin("once-retirement-claim-race", "1"),
                context: Arc::clone(&captured),
            }),
            Value::Null,
        )
        .await
        .unwrap();
    assert!(matches!(
        fiber.wait_settled().await.state,
        FiberState::Active
    ));
    let context = captured
        .lock()
        .expect("context capture poisoned")
        .clone()
        .expect("activation captures its Context");
    let callbacks = Arc::new(AtomicUsize::new(0));
    let listener = context
        .on(
            "once-retirement-claim-race",
            Arc::new(CountHandler(Arc::clone(&callbacks))),
            EventOptions {
                once: true,
                ..EventOptions::default()
            },
        )
        .unwrap();

    let cleanup_entered = Arc::new(Notify::new());
    let cleanup_release = Arc::new(Notify::new());
    let mut blocker = context.begin_effect("retirement claim barrier").unwrap();
    blocker
        .defer("retirement claim barrier", {
            let cleanup_entered = Arc::clone(&cleanup_entered);
            let cleanup_release = Arc::clone(&cleanup_release);
            Box::new(move || {
                async move {
                    cleanup_entered.notify_one();
                    cleanup_release.notified().await;
                    Ok(())
                }
                .boxed()
            })
        })
        .unwrap();
    let _blocker = blocker.commit().unwrap();

    let once_claim: OnceClaim = listener
        .begin_once_claim()
        .expect("the exact once removal claim succeeds");
    assert_eq!(runtime.resource_snapshot().listeners.current, 0);

    let disposing = tokio::spawn(async move { fiber.dispose().await });
    tokio::time::timeout(
        std::time::Duration::from_secs(2),
        cleanup_entered.notified(),
    )
    .await
    .expect("retirement claims every effect before running the LIFO barrier");

    assert!(
        !tokio::time::timeout(std::time::Duration::from_secs(2), once_claim.finish())
            .await
            .expect("a lost effect claim must not join retirement cleanup")
    );
    assert_eq!(callbacks.load(Ordering::Acquire), 0);
    cleanup_release.notify_one();
    assert!(
        tokio::time::timeout(std::time::Duration::from_secs(2), disposing)
            .await
            .expect("retirement completes after the deterministic barrier")
            .unwrap()
            .is_clean()
    );
    let complete = runtime.resource_snapshot();
    assert_eq!(complete.listeners.current, 0);
    assert_eq!(complete.effects.current, 0);
    assert_eq!(complete.effect_transactions.current, 0);
}
