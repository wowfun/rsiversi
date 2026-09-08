use rsi_api_protocol::{ApiError, Result};
use serde::{Deserialize, Serialize};
use std::{net::SocketAddr, path::PathBuf};

/// Explicit PEM assets for production TLS; configuration contains paths, never keys.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TlsFiles {
    /// PEM certificate chain, at most 1 MiB.
    pub certificate: PathBuf,
    /// PEM private key, at most 1 MiB.
    pub key: PathBuf,
}
/// Explicit listener and browser trust policy.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct HttpConfig {
    /// Requested native listening address.
    pub bind: SocketAddr,
    /// Exact externally served origin, including any non-default port.
    pub public_origin: String,
    /// Production certificates; absent only for explicit loopback development.
    pub tls: Option<TlsFiles>,
    /// Explicit opt-in to insecure loopback development HTTP.
    #[serde(default)]
    pub allow_loopback_http: bool,
}

#[derive(Clone, Debug)]
pub(crate) struct Policy {
    pub origin: String,
    pub authority: String,
    pub secure: bool,
}
impl HttpConfig {
    /// Validates the trust boundary before opening a socket or TLS file.
    pub fn validate(&self) -> Result<()> {
        self.policy().map(|_| ())
    }

    pub(crate) fn policy(&self) -> Result<Policy> {
        if self.public_origin.len() > 2048 {
            return Err(ApiError::Invalid("public origin is too long".into()));
        }
        let canonical = url::Url::parse(&self.public_origin)
            .map_err(|_| ApiError::Invalid("public origin is invalid".into()))?;
        if canonical.origin().ascii_serialization() != self.public_origin
            || !canonical.username().is_empty()
            || canonical.password().is_some()
            || canonical.query().is_some()
            || canonical.fragment().is_some()
            || canonical.path() != "/"
        {
            return Err(ApiError::Invalid(
                "public origin must be a canonical scheme and authority".into(),
            ));
        }
        let uri: http::Uri = self
            .public_origin
            .parse()
            .map_err(|_| ApiError::Invalid("public origin is invalid".into()))?;
        let authority = uri
            .authority()
            .ok_or_else(|| ApiError::Invalid("public origin requires an authority".into()))?;
        if authority.as_str().contains('@')
            || uri.query().is_some()
            || !matches!(uri.path(), "" | "/")
        {
            return Err(ApiError::Invalid(
                "public origin must contain only scheme and authority".into(),
            ));
        }
        let secure = self.tls.is_some();
        if secure {
            if self.allow_loopback_http || uri.scheme_str() != Some("https") {
                return Err(ApiError::Invalid(
                    "TLS requires an HTTPS origin and no insecure opt-in".into(),
                ));
            }
        } else if !self.allow_loopback_http
            || !self.bind.ip().is_loopback()
            || uri.scheme_str() != Some("http")
            || !matches!(authority.host(), "localhost" | "127.0.0.1" | "[::1]")
        {
            return Err(ApiError::Invalid(
                "HTTP requires explicit loopback development configuration".into(),
            ));
        }
        Ok(Policy {
            origin: format!("{}://{authority}", if secure { "https" } else { "http" }),
            authority: authority.as_str().into(),
            secure,
        })
    }
}

pub(crate) fn one<'a>(headers: &'a http::HeaderMap, name: &str) -> Result<Option<&'a str>> {
    let mut values = headers.get_all(name).iter();
    let value = values
        .next()
        .map(|value| value.to_str().map_err(|_| ApiError::Unauthorized))
        .transpose()?;
    if values.next().is_some() {
        return Err(ApiError::Unauthorized);
    }
    Ok(value)
}

impl Policy {
    pub fn fence(
        &self,
        headers: &http::HeaderMap,
        uri: &http::Uri,
        version: http::Version,
    ) -> Result<()> {
        let host = one(headers, "host")?;
        if version == http::Version::HTTP_2 {
            if !self.secure
                || uri.scheme_str() != Some("https")
                || uri.authority().map(http::uri::Authority::as_str)
                    != Some(self.authority.as_str())
                || host.is_some_and(|host| host != self.authority)
            {
                return Err(ApiError::Unauthorized);
            }
        } else if host != Some(self.authority.as_str()) {
            return Err(ApiError::Unauthorized);
        }
        if one(headers, "origin")?.is_some_and(|origin| origin != self.origin) {
            return Err(ApiError::Unauthorized);
        }
        Ok(())
    }
    pub fn browser(&self, headers: &http::HeaderMap) -> Result<()> {
        if one(headers, "origin")? != Some(self.origin.as_str())
            || one(headers, "x-rsi-csrf")? != Some("1")
        {
            return Err(ApiError::Unauthorized);
        }
        Ok(())
    }
}
