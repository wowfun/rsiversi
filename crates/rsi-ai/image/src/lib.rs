//! Exact-route Image router ordinary plugin.

#![deny(unsafe_code)]
#![warn(missing_docs)]
#![allow(clippy::missing_errors_doc)]

use async_trait::async_trait;
use rsi_ai_protocol::{
    AiCapability, AiError, DispatchStatus, ErrorKind, ErrorPhase, ImageCall, ImageCallContract,
    ImageRequest, ImageStream, ModelRef, PreparedCallSnapshot, PreparedImageCall,
    sanitize_error_summary,
};
use rsi_ai_provider::{
    AbortSignal, DurableMediaResolver, ImageAdapterStream, ImageRegistrar, ImageRegistrarContract,
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
        if route.registration.image().is_none() {
            return Err(invalid(format!(
                "AI deployment `{}` has no Image facet",
                model.deployment()
            )));
        }
        Ok(route)
    }

    async fn prepare_context(
        &self,
        registration: &ProviderRegistration,
        model: &ModelRef,
        request: &ImageRequest,
    ) -> Result<PrepareContext, AiError> {
        registration
            .image()
            .expect("route checked its Image facet")
            .validate_request(model.model(), request)?;
        let media_admission_bytes = image_media_admission_bytes(request)?;
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
            .map_err(|_| invalid("Image call identity exhausted"))?
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
        let media: Arc<dyn MediaResolver> = if !request.inputs().is_empty()
            || request.mask().is_some()
        {
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
            capability: AiCapability::Image,
            model: model.model().to_owned(),
            protocol: registration.image_protocol().to_owned(),
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
}

fn image_media_admission_bytes(request: &ImageRequest) -> Result<u64, AiError> {
    let mut unique = request.inputs().iter().collect::<HashSet<_>>();
    if let Some(mask) = request.mask() {
        unique.insert(mask);
    }
    unique.into_iter().try_fold(0_u64, |total, descriptor| {
        total
            .checked_add(descriptor.byte_len())
            .ok_or_else(|| artifact("Image media byte total overflowed"))
    })
}

#[async_trait]
impl ImageCall for Router {
    fn describe(&self, model: &ModelRef) -> Result<(), AiError> {
        self.route(model).map(|_| ())
    }

    async fn prepare(
        &self,
        model: ModelRef,
        request: ImageRequest,
    ) -> Result<Box<dyn PreparedImageCall>, AiError> {
        let route = self.route(&model)?;
        let context = self
            .prepare_context(&route.registration, &model, &request)
            .await?;
        let expected_call = context.snapshot().clone();
        let prepared = route
            .registration
            .image()
            .expect("route checked its Image facet")
            .prepare(context, model.model().to_owned(), request)
            .await?;
        if prepared.snapshot() != &expected_call {
            return Err(invalid(
                "provider changed the router-owned prepared-call snapshot",
            ));
        }
        Ok(Box::new(PinnedImageCall { prepared }))
    }
}

impl ImageRegistrar for Router {
    fn register_image(
        &self,
        registration: Arc<ProviderRegistration>,
        gate: RegistrationGate,
    ) -> Result<ProviderLease, ProviderSdkError> {
        if registration.image().is_none() {
            return Err(ProviderSdkError::new(
                "provider.missing_image_facet",
                "Image registrar received a registration without an Image adapter",
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
                format!("Image route `{deployment}` is already registered"),
            ));
        }
        inner.next_registration = inner.next_registration.checked_add(1).ok_or_else(|| {
            ProviderSdkError::new(
                "provider.registration_exhausted",
                "Image registration identity exhausted",
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
struct PinnedImageCall {
    prepared: Prepared<ImageAdapterStream>,
}

#[async_trait]
impl PreparedImageCall for PinnedImageCall {
    fn snapshot(&self) -> &PreparedCallSnapshot {
        self.prepared.snapshot()
    }

    async fn start(
        self: Box<Self>,
        cancellation: CancellationToken,
    ) -> Result<ImageStream, AiError> {
        self.prepared
            .start(AbortSignal::from_cancellation_token(cancellation))
            .await
    }
}

/// Ordinary factory for one exact-route Image router generation.
#[derive(Clone, Debug, Default)]
pub struct ImageRouterFactory;

#[async_trait]
impl PluginFactory for ImageRouterFactory {
    fn prepare(&self, desired: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        if !desired.is_null() && !desired.as_object().is_some_and(serde_json::Map::is_empty) {
            return Err(MetaError::InvalidInput(
                "Image router configuration must be null or empty".into(),
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
        let calls: Arc<dyn ImageCall> = router.clone();
        let registrar: Arc<dyn ImageRegistrar> = router;
        let call_supply = plan.context().provide_local::<ImageCallContract>(calls)?;
        let registrar_supply = match plan
            .context()
            .provide_local::<ImageRegistrarContract>(registrar)
        {
            Ok(supply) => supply,
            Err(error) => {
                drop(call_supply);
                return Err(error);
            }
        };
        plan.defer(
            "withdraw Image router",
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
