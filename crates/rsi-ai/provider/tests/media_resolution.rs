use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use std::time::Duration;

use rsi_ai_protocol::{
    AiCapability, AiError, DispatchStatus, ErrorKind, ErrorPhase, MediaDescriptor, MediaKind,
    PreparedCallSnapshot, RetryPolicy,
};
use rsi_ai_provider::{
    AbortSignal, AdapterFuture, MediaResolver, MissingMediaResolver, PrepareContext,
};
use sha2::{Digest, Sha256};
use tokio::sync::Semaphore;

fn descriptor(bytes: &[u8]) -> MediaDescriptor {
    MediaDescriptor::new(
        MediaKind::Image,
        "image/png",
        u64::try_from(bytes.len()).expect("test body length"),
        hex::encode(Sha256::digest(bytes)),
    )
    .expect("descriptor")
}

fn snapshot() -> PreparedCallSnapshot {
    PreparedCallSnapshot {
        call_id: "1".to_owned(),
        deployment_id: "deployment".to_owned(),
        provider_family: "provider".to_owned(),
        capability: AiCapability::Language,
        model: "model".to_owned(),
        protocol: "protocol".to_owned(),
        transport: "transport".to_owned(),
        endpoint_fingerprint: "endpoint".to_owned(),
        config_generation: 1,
        credential_source: None,
        retry_policy: RetryPolicy::default(),
        request_sha256: "0".repeat(64),
    }
}

#[test]
fn prepare_context_rejects_an_invalid_public_snapshot() {
    let mut invalid = snapshot();
    invalid.request_sha256 = "short".to_owned();

    assert!(
        PrepareContext::new(invalid, None, Arc::new(MissingMediaResolver), 0).is_err(),
        "provider context construction owns snapshot validation"
    );
}

#[derive(Debug)]
struct BlockingResolver {
    body: Arc<[u8]>,
    calls: AtomicUsize,
    started: Arc<Semaphore>,
    release: Arc<Semaphore>,
}

impl MediaResolver for BlockingResolver {
    fn read(
        &self,
        _descriptor: MediaDescriptor,
        _abort: AbortSignal,
    ) -> AdapterFuture<Result<Arc<[u8]>, AiError>> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.started.add_permits(1);
        let body = Arc::clone(&self.body);
        let release = Arc::clone(&self.release);
        Box::pin(async move {
            release
                .acquire()
                .await
                .expect("test resolver remains open")
                .forget();
            Ok(body)
        })
    }
}

#[tokio::test]
async fn identical_media_resolution_is_single_flight_within_one_prepared_call() {
    let body: Arc<[u8]> = Arc::from(b"same media".as_slice());
    let descriptor = descriptor(&body);
    let resolver = Arc::new(BlockingResolver {
        body,
        calls: AtomicUsize::new(0),
        started: Arc::new(Semaphore::new(0)),
        release: Arc::new(Semaphore::new(0)),
    });
    let context = PrepareContext::new(snapshot(), None, resolver.clone(), descriptor.byte_len())
        .expect("context");

    let first = tokio::spawn({
        let context = context.clone();
        let descriptor = descriptor.clone();
        async move { context.resolve_media(&descriptor, AbortSignal::new()).await }
    });
    let second = tokio::spawn({
        let context = context.clone();
        let descriptor = descriptor.clone();
        async move { context.resolve_media(&descriptor, AbortSignal::new()).await }
    });

    resolver
        .started
        .acquire()
        .await
        .expect("one resolver call starts")
        .forget();
    resolver.release.add_permits(1);
    let (first, second) = tokio::join!(first, second);
    let first = first.expect("first task").expect("first media");
    let second = second.expect("second task").expect("second media");

    assert_eq!(resolver.calls.load(Ordering::SeqCst), 1);
    assert!(Arc::ptr_eq(&first, &second));
}

#[tokio::test]
async fn a_waiter_can_cancel_without_waiting_for_the_shared_media_read() {
    let body: Arc<[u8]> = Arc::from(b"cancelled waiter".as_slice());
    let descriptor = descriptor(&body);
    let resolver = Arc::new(BlockingResolver {
        body,
        calls: AtomicUsize::new(0),
        started: Arc::new(Semaphore::new(0)),
        release: Arc::new(Semaphore::new(0)),
    });
    let context = PrepareContext::new(snapshot(), None, resolver.clone(), descriptor.byte_len())
        .expect("context");

    let first = tokio::spawn({
        let context = context.clone();
        let descriptor = descriptor.clone();
        async move { context.resolve_media(&descriptor, AbortSignal::new()).await }
    });
    resolver
        .started
        .acquire()
        .await
        .expect("shared resolver call starts")
        .forget();

    let waiter_abort = AbortSignal::new();
    let waiter = tokio::spawn({
        let context = context.clone();
        let descriptor = descriptor.clone();
        let abort = waiter_abort.clone();
        async move { context.resolve_media(&descriptor, abort).await }
    });
    waiter_abort.abort();
    let error = tokio::time::timeout(Duration::from_secs(1), waiter)
        .await
        .expect("cancelled waiter returns while shared read is pending")
        .expect("waiter task")
        .expect_err("waiter is cancelled");
    assert_eq!(error.kind(), ErrorKind::Cancelled);
    assert_eq!(resolver.calls.load(Ordering::SeqCst), 1);

    resolver.release.add_permits(1);
    first.await.expect("first task").expect("first media");
}

#[tokio::test]
async fn cancelling_the_first_waiter_does_not_cancel_the_shared_media_read() {
    let body: Arc<[u8]> = Arc::from(b"independent shared read".as_slice());
    let descriptor = descriptor(&body);
    let resolver = Arc::new(BlockingResolver {
        body,
        calls: AtomicUsize::new(0),
        started: Arc::new(Semaphore::new(0)),
        release: Arc::new(Semaphore::new(0)),
    });
    let context = PrepareContext::new(snapshot(), None, resolver.clone(), descriptor.byte_len())
        .expect("context");

    let first_abort = AbortSignal::new();
    let first = tokio::spawn({
        let context = context.clone();
        let descriptor = descriptor.clone();
        let abort = first_abort.clone();
        async move { context.resolve_media(&descriptor, abort).await }
    });
    resolver
        .started
        .acquire()
        .await
        .expect("shared resolver call starts")
        .forget();
    let second = tokio::spawn({
        let context = context.clone();
        let descriptor = descriptor.clone();
        async move { context.resolve_media(&descriptor, AbortSignal::new()).await }
    });

    first_abort.abort();
    let error = tokio::time::timeout(Duration::from_secs(1), first)
        .await
        .expect("first waiter observes its cancellation")
        .expect("first task")
        .expect_err("first waiter is cancelled");
    assert_eq!(error.kind(), ErrorKind::Cancelled);
    resolver.release.add_permits(1);
    second.await.expect("second task").expect("second media");
    assert_eq!(resolver.calls.load(Ordering::SeqCst), 1);
}

#[derive(Debug)]
struct FlakyResolver {
    body: Arc<[u8]>,
    calls: AtomicUsize,
}

impl MediaResolver for FlakyResolver {
    fn read(
        &self,
        _descriptor: MediaDescriptor,
        _abort: AbortSignal,
    ) -> AdapterFuture<Result<Arc<[u8]>, AiError>> {
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        let body = Arc::clone(&self.body);
        Box::pin(async move {
            if call == 0 {
                Err(AiError::new(
                    ErrorKind::Artifact,
                    ErrorPhase::Send,
                    DispatchStatus::NotDispatched,
                    "transient resolver failure",
                )
                .expect("static error"))
            } else {
                Ok(body)
            }
        })
    }
}

#[tokio::test]
async fn media_resolution_does_not_cache_failures_or_cross_prepared_calls() {
    let body: Arc<[u8]> = Arc::from(b"retry media".as_slice());
    let descriptor = descriptor(&body);
    let resolver = Arc::new(FlakyResolver {
        body,
        calls: AtomicUsize::new(0),
    });
    let first_call = PrepareContext::new(snapshot(), None, resolver.clone(), descriptor.byte_len())
        .expect("first context");

    assert!(
        first_call
            .resolve_media(&descriptor, AbortSignal::new())
            .await
            .is_err()
    );
    first_call
        .resolve_media(&descriptor, AbortSignal::new())
        .await
        .expect("failure is retried");

    let second_call =
        PrepareContext::new(snapshot(), None, resolver.clone(), descriptor.byte_len())
            .expect("second context");
    second_call
        .resolve_media(&descriptor, AbortSignal::new())
        .await
        .expect("another prepared call resolves independently");
    assert_eq!(resolver.calls.load(Ordering::SeqCst), 3);
}

#[tokio::test]
async fn releasing_a_completed_media_cache_forces_a_fresh_read() {
    let body: Arc<[u8]> = Arc::from(b"released media".as_slice());
    let descriptor = descriptor(&body);
    let resolver = Arc::new(FlakyResolver {
        body,
        calls: AtomicUsize::new(1),
    });
    let context = PrepareContext::new(snapshot(), None, resolver.clone(), descriptor.byte_len())
        .expect("context");

    context
        .resolve_media(&descriptor, AbortSignal::new())
        .await
        .expect("first resolution");
    context.release_resolved_media();
    context
        .resolve_media(&descriptor, AbortSignal::new())
        .await
        .expect("resolution after release");

    assert_eq!(resolver.calls.load(Ordering::SeqCst), 3);
}

#[tokio::test]
async fn media_resolution_rejects_declared_length_and_digest_mismatches() {
    let expected: Arc<[u8]> = Arc::from(b"expected body".as_slice());
    let descriptor = descriptor(&expected);
    for invalid in [
        Arc::<[u8]>::from(b"short".as_slice()),
        Arc::<[u8]>::from(b"tampered body".as_slice()),
    ] {
        let resolver = Arc::new(FlakyResolver {
            body: invalid,
            calls: AtomicUsize::new(1),
        });
        let context =
            PrepareContext::new(snapshot(), None, resolver, descriptor.byte_len()).unwrap();
        let error = context
            .resolve_media(&descriptor, AbortSignal::new())
            .await
            .expect_err("untrusted Media bytes must match the complete descriptor");
        assert_eq!(error.kind(), ErrorKind::Artifact);
    }
}
