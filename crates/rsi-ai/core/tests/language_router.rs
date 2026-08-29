use futures_util::{StreamExt as _, stream};
use rsi_ai::LanguageRouterFactory;
use rsi_ai_protocol::{
    ContentDelta, ContentStart, DeferredStatus as CallerDeferredStatus, DispatchStatus, ErrorKind,
    ErrorPhase, FinishReason, ImageToolResultCapability, LanguageCallContract, LanguageEvent,
    LanguageProfile, LanguageRequest, Message, ModelRef, RetryPolicy, ToolDialect,
};
use rsi_ai_provider::{
    AbortSignal, AdapterFuture, DeferredLanguageAdapterHandle, DeferredLanguageAdapterStream,
    DeferredLanguageBatch, DeferredLanguageCheckpoint, DeferredLanguageOperation, DeferredStatus,
    LanguageAdapter, LanguageAdapterStream, LanguageRegistrarContract, PrepareContext, Prepared,
    ProviderRegistration, RegistrationGate,
};
use rsi_credentials_protocol::CredentialRef;
use rsi_credentials_testkit::MemoryCredentialsFactory;
use rsi_meta::{ResolvedFactory, Runtime, UpdateMode};
use serde_json::Value;
use std::fmt;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use tokio_util::sync::CancellationToken;

fn linked(id: &str, factory: Arc<dyn rsi_meta::PluginFactory>) -> ResolvedFactory {
    ResolvedFactory::linked(id, "test", UpdateMode::Replayable, factory)
}

#[derive(Clone)]
struct Adapter {
    starts: Arc<AtomicUsize>,
    reject_request: bool,
    misreport_checkpoint: bool,
}

impl fmt::Debug for Adapter {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Adapter")
            .field("starts", &self.starts.load(Ordering::SeqCst))
            .field("reject_request", &self.reject_request)
            .field("misreport_checkpoint", &self.misreport_checkpoint)
            .finish()
    }
}

impl LanguageAdapter for Adapter {
    fn describe(&self, _model: &str) -> Result<LanguageProfile, rsi_ai_protocol::AiError> {
        Ok(LanguageProfile::new(
            8_192,
            512,
            1_024,
            ToolDialect::Responses,
            false,
            ImageToolResultCapability::No,
            Vec::new(),
        )
        .unwrap())
    }

    fn validate_request(
        &self,
        _model: &str,
        _request: &LanguageRequest,
    ) -> Result<(), rsi_ai_protocol::AiError> {
        if self.reject_request {
            return Err(rsi_ai_protocol::AiError::new(
                ErrorKind::Unsupported,
                ErrorPhase::Prepare,
                DispatchStatus::NotStarted,
                "scripted provider does not support this request",
            )
            .expect("static compatibility error"));
        }
        Ok(())
    }

    fn prepare(
        &self,
        context: PrepareContext,
        _model: String,
        _request: LanguageRequest,
    ) -> AdapterFuture<Result<Prepared<LanguageAdapterStream>, rsi_ai_protocol::AiError>> {
        if self.reject_request {
            return Box::pin(async {
                Err(rsi_ai_protocol::AiError::new(
                    ErrorKind::Unsupported,
                    ErrorPhase::Prepare,
                    DispatchStatus::NotStarted,
                    "scripted provider does not support this request",
                )
                .expect("static compatibility error"))
            });
        }
        let starts = Arc::clone(&self.starts);
        let mut snapshot = context.snapshot().clone();
        if self.misreport_checkpoint {
            snapshot.call_id = "another-valid-call".into();
        }
        Box::pin(async move {
            Ok(Prepared::new(snapshot, move |_abort| {
                starts.fetch_add(1, Ordering::SeqCst);
                Box::pin(async move {
                    let events = vec![LanguageEvent::Finished {
                        reason: FinishReason::Stop,
                        replay: None,
                    }];
                    Ok(Box::pin(stream::iter(events.into_iter().map(Ok))) as LanguageAdapterStream)
                })
            }))
        })
    }

    fn prepare_deferred(
        &self,
        context: PrepareContext,
        _model: String,
        _request: LanguageRequest,
    ) -> AdapterFuture<Result<Prepared<DeferredLanguageAdapterHandle>, rsi_ai_protocol::AiError>>
    {
        let starts = Arc::clone(&self.starts);
        let snapshot = context.snapshot().clone();
        let misreport_checkpoint = self.misreport_checkpoint;
        Box::pin(async move {
            let prepared_snapshot = snapshot.clone();
            Ok(Prepared::new(prepared_snapshot, move |_abort| {
                starts.fetch_add(1, Ordering::SeqCst);
                Box::pin(async move {
                    let mut snapshot = snapshot;
                    if misreport_checkpoint {
                        snapshot.call_id = "another-valid-call".into();
                    }
                    let checkpoint = DeferredLanguageCheckpoint::new(
                        snapshot,
                        "operation-1",
                        DeferredStatus::InProgress,
                        None,
                    )
                    .unwrap();
                    Ok(Box::new(DeferredFixture { checkpoint }) as DeferredLanguageAdapterHandle)
                })
            }))
        })
    }

    fn restore_deferred(
        &self,
        _context: PrepareContext,
        checkpoint: DeferredLanguageCheckpoint,
    ) -> AdapterFuture<Result<DeferredLanguageAdapterHandle, rsi_ai_protocol::AiError>> {
        Box::pin(async move {
            Ok(Box::new(DeferredFixture { checkpoint }) as DeferredLanguageAdapterHandle)
        })
    }
}

#[derive(Debug)]
struct DeferredFixture {
    checkpoint: DeferredLanguageCheckpoint,
}

#[async_trait::async_trait]
impl DeferredLanguageOperation for DeferredFixture {
    fn checkpoint(&self) -> DeferredLanguageCheckpoint {
        self.checkpoint.clone()
    }

    async fn poll(
        &mut self,
        _abort: AbortSignal,
    ) -> Result<DeferredStatus, rsi_ai_protocol::AiError> {
        Ok(self.checkpoint.status())
    }

    async fn resume(
        &mut self,
        _abort: AbortSignal,
    ) -> Result<DeferredLanguageAdapterStream, rsi_ai_protocol::AiError> {
        self.checkpoint
            .advance(DeferredStatus::Completed, true, 1, None)
            .unwrap();
        let events = vec![
            LanguageEvent::ContentStarted {
                index: 0,
                content: ContentStart::Text,
            },
            LanguageEvent::ContentDelta {
                index: 0,
                delta: ContentDelta::Text("done".into()),
            },
            LanguageEvent::ContentFinished { index: 0 },
            LanguageEvent::Finished {
                reason: FinishReason::Stop,
                replay: None,
            },
        ];
        let batch = DeferredLanguageBatch::new(events, self.checkpoint.clone()).unwrap();
        Ok(Box::pin(stream::iter([Ok(batch)])))
    }

    async fn cancel(
        &mut self,
        _abort: AbortSignal,
    ) -> Result<DeferredStatus, rsi_ai_protocol::AiError> {
        Ok(DeferredStatus::Cancelled)
    }
}

fn registration(generation: u64, starts: Arc<AtomicUsize>) -> Arc<ProviderRegistration> {
    registration_with_policy(generation, starts, RetryPolicy::default())
}

fn registration_with_policy(
    generation: u64,
    starts: Arc<AtomicUsize>,
    retry_policy: RetryPolicy,
) -> Arc<ProviderRegistration> {
    Arc::new(
        ProviderRegistration::builder("test", "scripted")
            .unwrap()
            .with_config_generation(generation)
            .with_retry_policy(retry_policy)
            .with_language(Adapter {
                starts,
                reject_request: false,
                misreport_checkpoint: false,
            })
            .build()
            .unwrap(),
    )
}

fn misreporting_registration(starts: Arc<AtomicUsize>) -> Arc<ProviderRegistration> {
    Arc::new(
        ProviderRegistration::builder("test", "scripted")
            .unwrap()
            .with_config_generation(1)
            .with_language(Adapter {
                starts,
                reject_request: false,
                misreport_checkpoint: true,
            })
            .build()
            .unwrap(),
    )
}

#[tokio::test]
async fn deferred_restore_rejects_retry_policy_drift_within_the_same_generation_number() {
    let runtime = Runtime::default();
    let credentials = runtime
        .root()
        .apply(
            linked("rsi.credentials.memory", Arc::new(MemoryCredentialsFactory)),
            Value::Null,
        )
        .await
        .unwrap();
    let router = runtime
        .root()
        .apply(
            linked("rsi.ai.language", Arc::new(LanguageRouterFactory)),
            Value::Null,
        )
        .await
        .unwrap();
    let calls = runtime
        .root()
        .lookup_local::<LanguageCallContract>()
        .unwrap();
    let registrar = runtime
        .root()
        .lookup_local::<LanguageRegistrarContract>()
        .unwrap();
    let first_gate = RegistrationGate::new();
    let first = registrar
        .register_language(
            registration(1, Arc::new(AtomicUsize::new(0))),
            first_gate.clone(),
        )
        .unwrap();
    first_gate.commit();
    let prepared = calls
        .prepare_deferred(
            ModelRef::new("test", "model").unwrap(),
            LanguageRequest::new(vec![Message::user_text("background").unwrap()]).unwrap(),
        )
        .await
        .unwrap();
    let operation = prepared.start(CancellationToken::new()).await.unwrap();
    let checkpoint = operation.checkpoint().unwrap();
    drop(first);

    let changed_policy = RetryPolicy::new(0, vec![ErrorKind::Transport], 1, 1, 0).unwrap();
    let second_gate = RegistrationGate::new();
    let second = registrar
        .register_language(
            registration_with_policy(1, Arc::new(AtomicUsize::new(0)), changed_policy),
            second_gate.clone(),
        )
        .unwrap();
    second_gate.commit();

    assert!(calls.restore_deferred(checkpoint).await.is_err());

    drop(second);
    drop(registrar);
    drop(calls);
    assert!(router.dispose().await.is_clean());
    assert!(credentials.dispose().await.is_clean());
}

#[tokio::test]
async fn deferred_provider_cannot_replace_the_pinned_call_snapshot_or_panic_the_caller() {
    let runtime = Runtime::default();
    let credentials = runtime
        .root()
        .apply(
            linked("rsi.credentials.memory", Arc::new(MemoryCredentialsFactory)),
            Value::Null,
        )
        .await
        .unwrap();
    let router = runtime
        .root()
        .apply(
            linked("rsi.ai.language", Arc::new(LanguageRouterFactory)),
            Value::Null,
        )
        .await
        .unwrap();
    let calls = runtime
        .root()
        .lookup_local::<LanguageCallContract>()
        .unwrap();
    let registrar = runtime
        .root()
        .lookup_local::<LanguageRegistrarContract>()
        .unwrap();
    let gate = RegistrationGate::new();
    let lease = registrar
        .register_language(
            misreporting_registration(Arc::new(AtomicUsize::new(0))),
            gate.clone(),
        )
        .unwrap();
    gate.commit();
    assert!(
        calls
            .prepare(
                ModelRef::new("test", "model").unwrap(),
                LanguageRequest::new(vec![Message::user_text("foreground").unwrap()]).unwrap(),
            )
            .await
            .is_err()
    );
    let prepared = calls
        .prepare_deferred(
            ModelRef::new("test", "model").unwrap(),
            LanguageRequest::new(vec![Message::user_text("background").unwrap()]).unwrap(),
        )
        .await
        .unwrap();
    let mut operation = prepared.start(CancellationToken::new()).await.unwrap();

    assert!(operation.checkpoint().is_err());
    let mut stream = operation.resume(CancellationToken::new()).await.unwrap();
    assert!(stream.next().await.unwrap().is_err());

    drop(lease);
    drop(registrar);
    drop(calls);
    assert!(router.dispose().await.is_clean());
    assert!(credentials.dispose().await.is_clean());
}

#[tokio::test]
async fn provider_compatibility_precedes_credential_resolution() {
    let runtime = Runtime::default();
    let credentials = runtime
        .root()
        .apply(
            linked("rsi.credentials.memory", Arc::new(MemoryCredentialsFactory)),
            Value::Null,
        )
        .await
        .unwrap();
    let router = runtime
        .root()
        .apply(
            linked("rsi.ai.language", Arc::new(LanguageRouterFactory)),
            Value::Null,
        )
        .await
        .unwrap();
    let calls = runtime
        .root()
        .lookup_local::<LanguageCallContract>()
        .unwrap();
    let registrar = runtime
        .root()
        .lookup_local::<LanguageRegistrarContract>()
        .unwrap();
    let registration = Arc::new(
        ProviderRegistration::builder("rejecting", "scripted")
            .unwrap()
            .with_config_generation(1)
            .with_credential(CredentialRef::new("provider", "missing").unwrap())
            .with_language(Adapter {
                starts: Arc::new(AtomicUsize::new(0)),
                reject_request: true,
                misreport_checkpoint: false,
            })
            .build()
            .unwrap(),
    );
    let gate = RegistrationGate::new();
    let lease = registrar
        .register_language(registration, gate.clone())
        .unwrap();
    gate.commit();

    let error = calls
        .prepare(
            ModelRef::new("rejecting", "model").unwrap(),
            LanguageRequest::new(vec![Message::user_text("hello").unwrap()]).unwrap(),
        )
        .await
        .expect_err("provider rejects before the missing credential is read");
    assert_eq!(error.kind(), ErrorKind::Unsupported);

    drop(lease);
    drop(registrar);
    drop(calls);
    assert!(router.dispose().await.is_clean());
    assert!(credentials.dispose().await.is_clean());
}

#[tokio::test]
async fn route_visibility_is_gate_atomic_and_prepared_call_pins_provider_generation() {
    let runtime = Runtime::default();
    let credentials = runtime
        .root()
        .apply(
            linked("rsi.credentials.memory", Arc::new(MemoryCredentialsFactory)),
            Value::Null,
        )
        .await
        .unwrap();
    let router = runtime
        .root()
        .apply(
            linked("rsi.ai.language", Arc::new(LanguageRouterFactory)),
            Value::Null,
        )
        .await
        .unwrap();
    let calls = runtime
        .root()
        .lookup_local::<LanguageCallContract>()
        .unwrap();
    let registrar = runtime
        .root()
        .lookup_local::<LanguageRegistrarContract>()
        .unwrap();
    let model = ModelRef::new("test", "model").unwrap();
    let first_starts = Arc::new(AtomicUsize::new(0));
    let gate = RegistrationGate::new();
    let first = registrar
        .register_language(registration(1, Arc::clone(&first_starts)), gate.clone())
        .unwrap();
    assert!(calls.describe(&model).is_err());
    gate.commit();
    assert_eq!(
        calls.describe(&model).unwrap().context_window_tokens(),
        8_192
    );

    let request = LanguageRequest::new(vec![Message::user_text("hello").unwrap()]).unwrap();
    let prepared = calls.prepare(model.clone(), request.clone()).await.unwrap();
    assert_eq!(prepared.snapshot().config_generation, 1);
    drop(first);

    let second_starts = Arc::new(AtomicUsize::new(0));
    let second_gate = RegistrationGate::new();
    let second = registrar
        .register_language(
            registration(2, Arc::clone(&second_starts)),
            second_gate.clone(),
        )
        .unwrap();
    second_gate.commit();
    let mut stream = prepared.start(CancellationToken::new()).await.unwrap();
    assert!(matches!(
        stream.next().await.unwrap().unwrap(),
        LanguageEvent::Finished { .. }
    ));
    assert_eq!(first_starts.load(Ordering::SeqCst), 1);
    assert_eq!(second_starts.load(Ordering::SeqCst), 0);
    assert_eq!(
        calls
            .prepare(model.clone(), request)
            .await
            .unwrap()
            .snapshot()
            .config_generation,
        2
    );
    drop(second);
    assert!(calls.describe(&model).is_err());

    drop(registrar);
    drop(calls);
    assert!(router.dispose().await.is_clean());
    assert!(credentials.dispose().await.is_clean());
}

#[tokio::test]
async fn deferred_submission_resume_and_restore_are_reachable_through_language_call() {
    let runtime = Runtime::default();
    let credentials = runtime
        .root()
        .apply(
            linked("rsi.credentials.memory", Arc::new(MemoryCredentialsFactory)),
            Value::Null,
        )
        .await
        .unwrap();
    let router = runtime
        .root()
        .apply(
            linked("rsi.ai.language", Arc::new(LanguageRouterFactory)),
            Value::Null,
        )
        .await
        .unwrap();
    let calls = runtime
        .root()
        .lookup_local::<LanguageCallContract>()
        .unwrap();
    let registrar = runtime
        .root()
        .lookup_local::<LanguageRegistrarContract>()
        .unwrap();
    let gate = RegistrationGate::new();
    let lease = registrar
        .register_language(registration(1, Arc::new(AtomicUsize::new(0))), gate.clone())
        .unwrap();
    gate.commit();
    let model = ModelRef::new("test", "model").unwrap();
    let request = LanguageRequest::new(vec![Message::user_text("background").unwrap()]).unwrap();
    let prepared = calls.prepare_deferred(model, request).await.unwrap();
    assert_eq!(prepared.snapshot().config_generation, 1);
    let mut operation = prepared.start(CancellationToken::new()).await.unwrap();
    assert_eq!(
        operation.poll(CancellationToken::new()).await.unwrap(),
        CallerDeferredStatus::InProgress
    );
    let initial = operation.checkpoint().unwrap();
    let restored = calls.restore_deferred(initial).await.unwrap();
    assert_eq!(restored.checkpoint().unwrap().call().config_generation, 1);
    let mut stream = operation.resume(CancellationToken::new()).await.unwrap();
    let batch = stream.next().await.unwrap().unwrap();
    assert_eq!(batch.checkpoint().sequence_number(), Some(1));
    assert!(batch.checkpoint().event_stream_terminal());

    drop(lease);
    let replacement_gate = RegistrationGate::new();
    let replacement = registrar
        .register_language(
            registration(2, Arc::new(AtomicUsize::new(0))),
            replacement_gate.clone(),
        )
        .unwrap();
    replacement_gate.commit();
    assert!(
        calls
            .restore_deferred(batch.checkpoint().clone())
            .await
            .is_err()
    );

    drop(replacement);
    drop(registrar);
    drop(calls);
    assert!(router.dispose().await.is_clean());
    assert!(credentials.dispose().await.is_clean());
}
