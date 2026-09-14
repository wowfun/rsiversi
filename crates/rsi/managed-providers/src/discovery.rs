use super::{ApiError, Arc, CallOrigin, ManagedProviders, ProviderKind, Result, Semaphore};
use rsi_ai_transport::HttpTransport;
use rsi_configuration_api::{DiscoveryRequest, DiscoverySnapshot};
use rsi_credentials_protocol::{
    CredentialRef, CredentialStoreFailure, CredentialsError, CredentialsResolve,
};
use tokio_util::sync::CancellationToken;

#[derive(Debug)]
pub(super) struct Discovery {
    credentials: Arc<dyn CredentialsResolve>,
    transport: Arc<dyn HttpTransport>,
    slots: Semaphore,
    stop: CancellationToken,
}
impl Discovery {
    pub fn new(
        credentials: Arc<dyn CredentialsResolve>,
        transport: Arc<dyn HttpTransport>,
    ) -> Self {
        Self {
            credentials,
            transport,
            slots: Semaphore::new(2),
            stop: CancellationToken::new(),
        }
    }
    pub fn close(&self) {
        self.stop.cancel();
        self.slots.close();
    }
    async fn run(&self, request: DiscoveryRequest) -> Result<DiscoverySnapshot> {
        if self.stop.is_cancelled() {
            return Err(ApiError::ShuttingDown);
        }
        let _permit = self.slots.try_acquire().map_err(|error| match error {
            tokio::sync::TryAcquireError::Closed => ApiError::ShuttingDown,
            tokio::sync::TryAcquireError::NoPermits => ApiError::Capacity,
        })?;
        let work = async {
            let reference = CredentialRef::new(request.provider.owner(), &request.slot)
                .map_err(|_| ApiError::Invalid("invalid discovery credential".into()))?;
            let credential = self
                .credentials
                .resolve(&reference)
                .await
                .map_err(|error| credential_error(&error))?;
            let transport = self.transport.as_ref();
            let models = match request.provider {
                ProviderKind::Openai => {
                    rsi_ai_openai::discovery::discover_models(
                        transport,
                        &request.endpoint,
                        &credential.secret,
                    )
                    .await
                }
                ProviderKind::Deepseek => {
                    rsi_ai_deepseek::discover_models(
                        transport,
                        &request.endpoint,
                        &credential.secret,
                    )
                    .await
                }
                ProviderKind::OpenaiCompatible => {
                    rsi_ai_openai_compatible::discover_models(
                        transport,
                        &request.endpoint,
                        &credential.secret,
                    )
                    .await
                }
            }
            .map_err(|error| provider_error(&error))?;
            Ok(DiscoverySnapshot { request, models })
        };
        tokio::select! { biased;
            () = self.stop.cancelled() => Err(ApiError::ShuttingDown),
            result = tokio::time::timeout(std::time::Duration::from_secs(30), work) => result.unwrap_or_else(|_| Err(ApiError::Backend("Model discovery timed out; retry or enter a model manually".into()))),
        }
    }
}
fn credential_error(error: &CredentialsError) -> ApiError {
    match error {
        CredentialsError::NotConfigured(_) | CredentialsError::InvalidInput(_) => {
            ApiError::Invalid(
                "Provider credential unavailable; use /login or configure the Host environment"
                    .into(),
            )
        }
        CredentialsError::Store(CredentialStoreFailure::LockTimeout) => ApiError::Capacity,
        _ => ApiError::Backend(
            "Provider credential lookup failed; retry after checking Host credential status".into(),
        ),
    }
}
fn provider_error(error: &rsi_ai_protocol::AiError) -> ApiError {
    use rsi_ai_protocol::ErrorKind;
    match error.kind() {
        ErrorKind::RateLimited => ApiError::Capacity,
        ErrorKind::InvalidRequest | ErrorKind::Authentication | ErrorKind::Permission => {
            ApiError::Invalid(error.safe_summary().into())
        }
        _ => ApiError::Backend(error.safe_summary().into()),
    }
}
impl ManagedProviders {
    /// Resolves one provider-owned secret under configuration authority and bounded admission.
    pub async fn discover(
        &self,
        origin: &CallOrigin,
        request: DiscoveryRequest,
    ) -> Result<DiscoverySnapshot> {
        let _lease = self.access.admit(origin)?;
        request.validate()?;
        self.discovery.run(request).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use rsi_ai_transport::{HttpRequest, HttpResponse, TransportError};
    use rsi_credentials_protocol::{CredentialSource, ResolvedCredential, SecretValue};
    use std::sync::Mutex;
    #[derive(Debug)]
    struct Credentials;
    #[async_trait]
    impl CredentialsResolve for Credentials {
        async fn resolve(
            &self,
            _: &CredentialRef,
        ) -> rsi_credentials_protocol::Result<ResolvedCredential> {
            Ok(ResolvedCredential {
                secret: SecretValue::new("test-only-secret").unwrap(),
                source: CredentialSource::Environment {
                    variable: "TEST_KEY".into(),
                },
            })
        }
    }
    #[derive(Debug, Default)]
    struct Pending(Mutex<Vec<CancellationToken>>);
    #[async_trait]
    impl HttpTransport for Pending {
        async fn execute(
            &self,
            _: HttpRequest,
            stop: CancellationToken,
        ) -> std::result::Result<HttpResponse, TransportError> {
            self.0.lock().unwrap().push(stop);
            std::future::pending().await
        }
    }
    fn request() -> DiscoveryRequest {
        DiscoveryRequest {
            provider: ProviderKind::Deepseek,
            endpoint: "https://mock.invalid".into(),
            slot: "default".into(),
        }
    }
    #[test]
    fn credential_failures_separate_bad_setup_from_temporary_backend_failure() {
        assert!(matches!(
            credential_error(&CredentialsError::NotConfigured("slot".into())),
            ApiError::Invalid(_)
        ));
        assert!(matches!(
            credential_error(&CredentialsError::Store(
                CredentialStoreFailure::LockTimeout
            )),
            ApiError::Capacity
        ));
        assert!(matches!(
            credential_error(&CredentialsError::Timeout("slot".into())),
            ApiError::Backend(_)
        ));
        assert!(matches!(
            credential_error(&CredentialsError::Store(CredentialStoreFailure::Io)),
            ApiError::Backend(_)
        ));
    }
    #[tokio::test]
    async fn closed_admission_is_shutdown_even_before_cancellation_is_observed() {
        let transport = Arc::new(Pending::default());
        let owner = Discovery::new(Arc::new(Credentials), transport.clone());
        owner.slots.close();
        assert!(matches!(
            owner.run(request()).await,
            Err(ApiError::ShuttingDown)
        ));
        assert!(transport.0.lock().unwrap().is_empty());
    }
    #[tokio::test(start_paused = true)]
    async fn whole_request_timeout_cancels_transport_and_releases_capacity() {
        let transport = Arc::new(Pending::default());
        let owner = Discovery::new(Arc::new(Credentials), transport.clone());
        let start = tokio::time::Instant::now();
        assert!(
            matches!(owner.run(request()).await, Err(ApiError::Backend(message)) if message.contains("timed out"))
        );
        assert_eq!(start.elapsed(), std::time::Duration::from_secs(30));
        assert!(transport.0.lock().unwrap()[0].is_cancelled());
        assert_eq!(owner.slots.available_permits(), 2);
    }
    #[tokio::test(start_paused = true)]
    async fn concurrency_cancellation_and_shutdown_are_bounded() {
        let transport = Arc::new(Pending::default());
        let owner = Discovery::new(Arc::new(Credentials), transport.clone());
        let mut first = Box::pin(owner.run(request()));
        let mut second = Box::pin(owner.run(request()));
        assert!(futures_util::poll!(first.as_mut()).is_pending());
        assert!(futures_util::poll!(second.as_mut()).is_pending());
        assert!(matches!(
            owner.run(request()).await,
            Err(ApiError::Capacity)
        ));
        assert_eq!(transport.0.lock().unwrap().len(), 2);
        drop(first);
        assert!(transport.0.lock().unwrap()[0].is_cancelled());
        assert_eq!(owner.slots.available_permits(), 1);
        owner.close();
        assert!(matches!(second.await, Err(ApiError::ShuttingDown)));
        assert!(transport.0.lock().unwrap()[1].is_cancelled());
        assert!(matches!(
            owner.run(request()).await,
            Err(ApiError::ShuttingDown)
        ));
    }
}
