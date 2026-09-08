use rsi_api_protocol::{ApiError, EndpointId, Result};
use rsi_credentials_protocol::CredentialRef;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use tokio::io::AsyncReadExt;

/// Explicit native connection configuration; credential contents never enter this value.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HttpClientConfig {
    /// Canonical scheme and authority selected by the operator.
    pub origin: String,
    /// Expected persisted deployment identity.
    pub endpoint_id: EndpointId,
    /// Existing Credentials address, resolved only during activation.
    pub credential: CredentialRef,
    /// Explicit extra certificate authority, at most 1 MiB of PEM.
    pub tls_ca: Option<PathBuf>,
    /// Explicit opt-in restricted to loopback HTTP.
    #[serde(default)]
    pub allow_loopback_http: bool,
}
impl HttpClientConfig {
    /// Performs pure configuration validation before credentials or network access.
    pub fn validate(&self) -> Result<()> {
        self.credential
            .validate()
            .map_err(|_| ApiError::Invalid("invalid credential reference".into()))?;
        if self.origin.len() > 2048 {
            return Err(ApiError::Invalid("origin exceeds bound".into()));
        }
        let url = reqwest::Url::parse(&self.origin)
            .map_err(|_| ApiError::Invalid("invalid API origin".into()))?;
        if !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
            || url.path() != "/"
            || url.origin().ascii_serialization() != self.origin
        {
            return Err(ApiError::Invalid(
                "API origin must be a canonical scheme and authority".into(),
            ));
        }
        match url.scheme() {
            "https" if !self.allow_loopback_http => {}
            "http"
                if self.allow_loopback_http
                    && self.tls_ca.is_none()
                    && matches!(url.host_str(), Some("127.0.0.1" | "[::1]" | "localhost")) => {}
            _ => {
                return Err(ApiError::Invalid(
                    "HTTPS or explicit loopback HTTP is required".into(),
                ));
            }
        }
        Ok(())
    }
    pub(crate) async fn transport(&self) -> Result<reqwest::Client> {
        self.validate()?;
        let mut builder = reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .http1_only()
            .pool_max_idle_per_host(0)
            .no_gzip()
            .no_brotli()
            .no_deflate()
            .no_zstd()
            .connect_timeout(std::time::Duration::from_secs(10));
        if let Some(path) = &self.tls_ca {
            let mut options = tokio::fs::OpenOptions::new();
            options.read(true);
            #[cfg(unix)]
            options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC);
            let file = options
                .open(path)
                .await
                .map_err(|_| ApiError::Invalid("cannot open API CA file".into()))?;
            let metadata = file
                .metadata()
                .await
                .map_err(|_| ApiError::Invalid("cannot inspect API CA file".into()))?;
            if !metadata.is_file() || metadata.len() > 1024 * 1024 {
                return Err(ApiError::Invalid(
                    "API CA must be a regular file of at most 1 MiB".into(),
                ));
            }
            let mut bytes = Vec::with_capacity(1024 * 1024 + 1);
            file.take(1024 * 1024 + 1)
                .read_to_end(&mut bytes)
                .await
                .map_err(|_| ApiError::Invalid("cannot read API CA file".into()))?;
            if bytes.len() > 1024 * 1024 {
                return Err(ApiError::Invalid("API CA grew beyond bound".into()));
            }
            let certificates = reqwest::Certificate::from_pem_bundle(&bytes)
                .map_err(|_| ApiError::Invalid("invalid API CA certificates".into()))?;
            if certificates.is_empty() {
                return Err(ApiError::Invalid("API CA contains no certificate".into()));
            }
            for certificate in certificates {
                builder = builder.add_root_certificate(certificate);
            }
        }
        let url = reqwest::Url::parse(&self.origin).expect("validated origin");
        if url.scheme() == "http" && url.host_str() == Some("localhost") {
            builder = builder.resolve(
                "localhost",
                std::net::SocketAddr::from((
                    [127, 0, 0, 1],
                    url.port_or_known_default().expect("HTTP port"),
                )),
            );
        }
        builder
            .build()
            .map_err(|_| ApiError::Backend("cannot construct API transport".into()))
    }
}
