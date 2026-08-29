//! Exact-route Language router ordinary plugin.

#![deny(unsafe_code)]
#![warn(missing_docs)]
#![allow(clippy::missing_errors_doc)]

use async_trait::async_trait;
use futures_util::StreamExt as _;
use rsi_ai_protocol::{
    AiCapability, AiError, DeferredLanguageCall, DeferredLanguageCheckpoint,
    DeferredLanguageStream, DeferredStatus, DispatchStatus, ErrorKind, ErrorPhase, LanguageCall,
    LanguageCallContract, LanguageProfile, LanguageRequest, LanguageStream, MessageContent,
    ModelRef, PreparedCallSnapshot, PreparedDeferredLanguageCall, PreparedLanguageCall,
    sanitize_error_summary,
};
use rsi_ai_provider::{
    AbortSignal, DeferredLanguageAdapterHandle, DeferredLanguageCheckpoint as AdapterCheckpoint,
    DurableMediaResolver, LanguageAdapterStream, LanguageRegistrar, LanguageRegistrarContract,
    MediaResolver, MissingMediaResolver, PrepareContext, Prepared, ProviderLease,
    ProviderRegistration, ProviderSdkError, RegistrationGate, validate_media_admission_bytes,
};
use rsi_credentials_protocol::{CredentialsResolve, CredentialsResolveContract};
use rsi_media_protocol::MediaReadContract;
use rsi_meta::{
    ActivationPlan, ConfigValue, Context, MetaError, PluginFactory, PreparedActivation,
};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, Weak};
use tokio_util::sync::CancellationToken;

#[derive(Debug)]
struct Router {
    state: Arc<RouterState>,
    credentials: Arc<dyn CredentialsResolve>,
    context: Context,
    next_call_id: AtomicU64,
}

#[derive(Debug)]
struct RouterState {
    inner: Mutex<RouterInner>,
}

#[derive(Debug, Default)]
struct RouterInner {
    next_registration: u64,
    routes: BTreeMap<String, Route>,
}

#[derive(Clone, Debug)]
struct Route {
    registration_id: u64,
    registration: Arc<ProviderRegistration>,
    gate: RegistrationGate,
}

impl Router {
    fn route(&self, model: &ModelRef) -> Result<Route, AiError> {
        model
            .validate()
            .map_err(|error| invalid(error.to_string()))?;
        let route = self
            .state
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .routes
            .get(model.deployment())
            .filter(|route| route.gate.is_committed())
            .cloned()
            .ok_or_else(|| {
                invalid(format!(
                    "AI deployment `{}` is not registered",
                    model.deployment()
                ))
            })?;
        if route.registration.language().is_none() {
            return Err(invalid(format!(
                "AI deployment `{}` has no Language facet",
                model.deployment()
            )));
        }
        Ok(route)
    }

    async fn prepare_context(
        &self,
        registration: &ProviderRegistration,
        model: &ModelRef,
        request: &LanguageRequest,
    ) -> Result<PrepareContext, AiError> {
        registration
            .language()
            .expect("route checked its Language facet")
            .validate_request(model.model(), request)?;
        let media_admission_bytes = language_media_admission_bytes(request)?;
        validate_media_admission_bytes(media_admission_bytes)
            .map_err(|error| artifact(error.to_string()))?;
        let request_bytes = request
            .canonical_bytes()
            .map_err(|error| invalid(error.to_string()))?;
        let call_number = self
            .next_call_id
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |current| {
                current.checked_add(1)
            })
            .map_err(|_| invalid("Language call identity exhausted"))?
            + 1;
        let credential = match registration.credential() {
            Some(reference) => Some(
                self.credentials
                    .resolve(reference)
                    .await
                    .map_err(|error| authentication(error.to_string()))?,
            ),
            None => None,
        };
        let media: Arc<dyn MediaResolver> = if request_has_media(request) {
            let reader = self
                .context
                .lookup_local::<MediaReadContract>()
                .ok_or_else(|| artifact("request references Media but rsi.media.read is absent"))?;
            Arc::new(DurableMediaResolver::new(reader))
        } else {
            Arc::new(MissingMediaResolver)
        };
        let snapshot = PreparedCallSnapshot {
            call_id: format!("call-{call_number}"),
            deployment_id: registration.deployment_id().to_owned(),
            provider_family: registration.provider_family().to_owned(),
            capability: AiCapability::Language,
            model: model.model().to_owned(),
            protocol: registration.protocol().to_owned(),
            transport: registration.transport().to_owned(),
            endpoint_fingerprint: registration.endpoint_fingerprint().to_owned(),
            config_generation: registration.config_generation(),
            credential_source: credential
                .as_ref()
                .map(|credential| credential.source.clone()),
            retry_policy: registration.retry_policy().clone(),
            request_sha256: hex::encode(Sha256::digest(request_bytes)),
        };
        PrepareContext::new(snapshot, credential, media, media_admission_bytes)
            .map_err(|error| invalid(error.to_string()))
    }

    async fn restore_context(
        &self,
        registration: &ProviderRegistration,
        checkpoint: &DeferredLanguageCheckpoint,
    ) -> Result<PrepareContext, AiError> {
        checkpoint
            .validate()
            .map_err(|error| invalid(error.to_string()))?;
        let snapshot = checkpoint.call();
        if snapshot.capability != AiCapability::Language
            || snapshot.deployment_id != registration.deployment_id()
            || snapshot.provider_family != registration.provider_family()
            || snapshot.protocol != registration.protocol()
            || snapshot.transport != registration.transport()
            || snapshot.endpoint_fingerprint != registration.endpoint_fingerprint()
            || snapshot.config_generation != registration.config_generation()
            || snapshot.retry_policy != *registration.retry_policy()
        {
            return Err(invalid(
                "deferred checkpoint does not match the active Language provider generation",
            ));
        }
        let credential = match registration.credential() {
            Some(reference) => Some(
                self.credentials
                    .resolve(reference)
                    .await
                    .map_err(|error| authentication(error.to_string()))?,
            ),
            None => None,
        };
        if credential.as_ref().map(|credential| &credential.source)
            != snapshot.credential_source.as_ref()
        {
            return Err(authentication(
                "deferred checkpoint credential source does not match the active route",
            ));
        }
        PrepareContext::new(
            snapshot.clone(),
            credential,
            Arc::new(MissingMediaResolver),
            0,
        )
        .map_err(|error| invalid(error.to_string()))
    }
}

#[async_trait]
impl LanguageCall for Router {
    fn describe(&self, model: &ModelRef) -> Result<LanguageProfile, AiError> {
        let route = self.route(model)?;
        route
            .registration
            .language()
            .expect("route checked its Language facet")
            .describe(model.model())
    }

    async fn prepare(
        &self,
        model: ModelRef,
        request: LanguageRequest,
    ) -> Result<Box<dyn PreparedLanguageCall>, AiError> {
        let route = self.route(&model)?;
        let context = self
            .prepare_context(&route.registration, &model, &request)
            .await?;
        let expected_call = context.snapshot().clone();
        let prepared = route
            .registration
            .language()
            .expect("route checked its Language facet")
            .prepare(context, model.model().to_owned(), request)
            .await?;
        validate_prepared_snapshot(prepared.snapshot(), &expected_call)?;
        Ok(Box::new(PinnedLanguageCall { prepared }))
    }

    async fn prepare_deferred(
        &self,
        model: ModelRef,
        request: LanguageRequest,
    ) -> Result<Box<dyn PreparedDeferredLanguageCall>, AiError> {
        let route = self.route(&model)?;
        let context = self
            .prepare_context(&route.registration, &model, &request)
            .await?;
        let expected_call = context.snapshot().clone();
        let prepared = route
            .registration
            .language()
            .expect("route checked its Language facet")
            .prepare_deferred(context, model.model().to_owned(), request)
            .await?;
        validate_prepared_snapshot(prepared.snapshot(), &expected_call)?;
        Ok(Box::new(PinnedDeferredSubmission { prepared }))
    }

    async fn restore_deferred(
        &self,
        checkpoint: DeferredLanguageCheckpoint,
    ) -> Result<Box<dyn DeferredLanguageCall>, AiError> {
        let model = ModelRef::new(
            checkpoint.call().deployment_id.clone(),
            checkpoint.call().model.clone(),
        )
        .map_err(|error| invalid(error.to_string()))?;
        let route = self.route(&model)?;
        let context = self
            .restore_context(&route.registration, &checkpoint)
            .await?;
        let adapter_checkpoint = AdapterCheckpoint::from_caller(&checkpoint)
            .map_err(|error| invalid(error.to_string()))?;
        let operation = route
            .registration
            .language()
            .expect("route checked its Language facet")
            .restore_deferred(context, adapter_checkpoint)
            .await?;
        Ok(Box::new(PinnedDeferredOperation {
            operation,
            expected_call: checkpoint.call().clone(),
        }))
    }
}

impl LanguageRegistrar for Router {
    fn register_language(
        &self,
        registration: Arc<ProviderRegistration>,
        gate: RegistrationGate,
    ) -> Result<ProviderLease, ProviderSdkError> {
        if registration.language().is_none() {
            return Err(ProviderSdkError::new(
                "provider.missing_language_facet",
                "Language registrar received a registration without a Language adapter",
            ));
        }
        let deployment = registration.deployment_id().to_owned();
        let mut inner = self
            .state
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if inner.routes.contains_key(&deployment) {
            return Err(ProviderSdkError::new(
                "provider.duplicate_route",
                format!("Language route `{deployment}` is already registered"),
            ));
        }
        inner.next_registration = inner.next_registration.checked_add(1).ok_or_else(|| {
            ProviderSdkError::new(
                "provider.registration_exhausted",
                "Language registration identity exhausted",
            )
        })?;
        let registration_id = inner.next_registration;
        inner.routes.insert(
            deployment.clone(),
            Route {
                registration_id,
                registration,
                gate,
            },
        );
        let state = Arc::downgrade(&self.state);
        Ok(ProviderLease::new(move || {
            remove_route(&state, &deployment, registration_id);
        }))
    }
}

fn remove_route(state: &Weak<RouterState>, deployment: &str, registration_id: u64) {
    let Some(state) = state.upgrade() else {
        return;
    };
    let mut inner = state
        .inner
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if inner
        .routes
        .get(deployment)
        .is_some_and(|route| route.registration_id == registration_id)
    {
        inner.routes.remove(deployment);
    }
}

#[derive(Debug)]
struct PinnedLanguageCall {
    prepared: Prepared<LanguageAdapterStream>,
}

#[async_trait]
impl PreparedLanguageCall for PinnedLanguageCall {
    fn snapshot(&self) -> &PreparedCallSnapshot {
        self.prepared.snapshot()
    }

    async fn start(
        self: Box<Self>,
        cancellation: CancellationToken,
    ) -> Result<LanguageStream, AiError> {
        self.prepared
            .start(AbortSignal::from_cancellation_token(cancellation))
            .await
    }
}

#[derive(Debug)]
struct PinnedDeferredSubmission {
    prepared: Prepared<DeferredLanguageAdapterHandle>,
}

#[async_trait]
impl PreparedDeferredLanguageCall for PinnedDeferredSubmission {
    fn snapshot(&self) -> &PreparedCallSnapshot {
        self.prepared.snapshot()
    }

    async fn start(
        self: Box<Self>,
        cancellation: CancellationToken,
    ) -> Result<Box<dyn DeferredLanguageCall>, AiError> {
        let expected_call = self.prepared.snapshot().clone();
        let operation = self
            .prepared
            .start(AbortSignal::from_cancellation_token(cancellation))
            .await?;
        Ok(Box::new(PinnedDeferredOperation {
            operation,
            expected_call,
        }))
    }
}

#[derive(Debug)]
struct PinnedDeferredOperation {
    operation: DeferredLanguageAdapterHandle,
    expected_call: PreparedCallSnapshot,
}

#[async_trait]
impl DeferredLanguageCall for PinnedDeferredOperation {
    fn checkpoint(&self) -> Result<DeferredLanguageCheckpoint, AiError> {
        project_deferred_checkpoint(&self.operation.checkpoint(), &self.expected_call)
    }

    async fn poll(&mut self, cancellation: CancellationToken) -> Result<DeferredStatus, AiError> {
        self.operation
            .poll(AbortSignal::from_cancellation_token(cancellation))
            .await
            .map(Into::into)
    }

    async fn resume(
        &mut self,
        cancellation: CancellationToken,
    ) -> Result<DeferredLanguageStream, AiError> {
        let stream = self
            .operation
            .resume(AbortSignal::from_cancellation_token(cancellation))
            .await?;
        let expected_call = self.expected_call.clone();
        Ok(Box::pin(stream.map(move |batch| {
            batch.and_then(|batch| {
                let batch = batch
                    .to_caller()
                    .map_err(|error| invalid(error.to_string()))?;
                if batch.checkpoint().call() != &expected_call {
                    return Err(invalid(
                        "provider deferred batch changed its pinned prepared-call snapshot",
                    ));
                }
                Ok(batch)
            })
        })))
    }

    async fn cancel(&mut self, cancellation: CancellationToken) -> Result<DeferredStatus, AiError> {
        self.operation
            .cancel(AbortSignal::from_cancellation_token(cancellation))
            .await
            .map(Into::into)
    }
}

fn project_deferred_checkpoint(
    checkpoint: &AdapterCheckpoint,
    expected_call: &PreparedCallSnapshot,
) -> Result<DeferredLanguageCheckpoint, AiError> {
    let checkpoint = checkpoint
        .to_caller()
        .map_err(|error| invalid(error.to_string()))?;
    if checkpoint.call() != expected_call {
        return Err(invalid(
            "provider deferred checkpoint changed its pinned prepared-call snapshot",
        ));
    }
    Ok(checkpoint)
}

fn validate_prepared_snapshot(
    actual: &PreparedCallSnapshot,
    expected: &PreparedCallSnapshot,
) -> Result<(), AiError> {
    if actual != expected {
        return Err(invalid(
            "provider changed the router-owned prepared-call snapshot",
        ));
    }
    Ok(())
}

fn request_has_media(request: &LanguageRequest) -> bool {
    request
        .messages()
        .iter()
        .any(|message| message.content().iter().any(message_content_has_media))
}

fn language_media_admission_bytes(request: &LanguageRequest) -> Result<u64, AiError> {
    let mut unique = HashSet::new();
    for message in request.messages() {
        collect_media_descriptors(message.content(), &mut unique);
    }
    unique.into_iter().try_fold(0_u64, |total, descriptor| {
        total
            .checked_add(descriptor.byte_len())
            .ok_or_else(|| artifact("Language media byte total overflowed"))
    })
}

fn collect_media_descriptors<'a>(
    content: &'a [MessageContent],
    unique: &mut HashSet<&'a rsi_ai_protocol::MediaDescriptor>,
) {
    for block in content {
        match block {
            MessageContent::Image(descriptor) | MessageContent::Audio(descriptor) => {
                unique.insert(descriptor);
            }
            MessageContent::ToolResult { content, .. } => {
                collect_media_descriptors(content, unique);
            }
            MessageContent::Text { .. }
            | MessageContent::Reasoning { .. }
            | MessageContent::ToolCall(_) => {}
        }
    }
}

fn message_content_has_media(content: &MessageContent) -> bool {
    match content {
        MessageContent::Image(_) | MessageContent::Audio(_) => true,
        MessageContent::ToolResult { content, .. } => content.iter().any(message_content_has_media),
        MessageContent::Text { .. }
        | MessageContent::Reasoning { .. }
        | MessageContent::ToolCall(_) => false,
    }
}

/// Ordinary factory for one exact-route Language router generation.
#[derive(Clone, Debug, Default)]
pub struct LanguageRouterFactory;

#[async_trait]
impl PluginFactory for LanguageRouterFactory {
    fn prepare(&self, desired: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        if !desired.is_null() && !desired.as_object().is_some_and(serde_json::Map::is_empty) {
            return Err(MetaError::InvalidInput(
                "Language router configuration must be null or empty".into(),
            ));
        }
        Ok(PreparedActivation::new(Value::Null).requiring_local::<CredentialsResolveContract>())
    }

    async fn activate(&self, plan: ActivationPlan) -> rsi_meta::Result<()> {
        let router = Arc::new(Router {
            state: Arc::new(RouterState {
                inner: Mutex::new(RouterInner::default()),
            }),
            credentials: plan.local::<CredentialsResolveContract>()?,
            context: plan.context().clone(),
            next_call_id: AtomicU64::new(0),
        });
        let calls: Arc<dyn LanguageCall> = router.clone();
        let registrar: Arc<dyn LanguageRegistrar> = router;
        let call_supply = plan
            .context()
            .provide_local::<LanguageCallContract>(calls)?;
        let registrar_supply = match plan
            .context()
            .provide_local::<LanguageRegistrarContract>(registrar)
        {
            Ok(supply) => supply,
            Err(error) => {
                drop(call_supply);
                return Err(error);
            }
        };
        plan.defer(
            "withdraw Language router",
            Box::new(move || {
                Box::pin(async move {
                    drop(registrar_supply);
                    drop(call_supply);
                    Ok(())
                })
            }),
        )
    }
}

fn invalid(message: impl Into<String>) -> AiError {
    ai_error(ErrorKind::InvalidRequest, message)
}

fn authentication(message: impl Into<String>) -> AiError {
    ai_error(ErrorKind::Authentication, message)
}

fn artifact(message: impl Into<String>) -> AiError {
    ai_error(ErrorKind::Artifact, message)
}

fn ai_error(kind: ErrorKind, message: impl Into<String>) -> AiError {
    let message = sanitize_error_summary(&message.into());
    AiError::new(
        kind,
        ErrorPhase::Prepare,
        DispatchStatus::NotStarted,
        message,
    )
    .expect("router-generated AI errors are bounded static facts")
}
