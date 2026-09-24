use crate::server::{Body, full};
use http::{HeaderMap, Response};
use rsi_api_protocol::{ApiError, Result, RetainedBytes};
use rsi_meta::LocalContract;
use std::fmt;

/// Closed content types accepted for application assets.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AssetType {
    /// Application document.
    Html,
    /// External JavaScript module or Worker.
    JavaScript,
    /// External stylesheet.
    Css,
    /// WebAssembly module.
    Wasm,
    /// Application JSON data.
    Json,
    /// Raster PNG asset.
    Png,
}
impl AssetType {
    fn mime(self) -> &'static str {
        match self {
            Self::Html => "text/html; charset=utf-8",
            Self::JavaScript => "text/javascript; charset=utf-8",
            Self::Css => "text/css; charset=utf-8",
            Self::Wasm => "application/wasm",
            Self::Json => "application/json",
            Self::Png => "image/png",
        }
    }
}

/// Closed immutable document policies; products supply framed asset paths or directory prefixes.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize)]
pub enum DocumentPolicy {
    /// Trusted application; scripts remain external and only named documents may be framed.
    Application {
        /// Root asset paths or single-segment directory prefixes ending in `/`; never URL or CSP text.
        frame_paths: Vec<String>,
    },
    /// Opaque script-enabled document with local data resources only.
    SandboxLocal,
    /// Opaque script-enabled document with explicit HTTPS resource access.
    SandboxHttps,
}
impl Default for DocumentPolicy {
    fn default() -> Self {
        Self::Application {
            frame_paths: Vec::new(),
        }
    }
}
impl DocumentPolicy {
    /// Application requests retain their concrete Origin; opaque previews disclose no referrer.
    pub fn referrer_policy(&self) -> &'static str {
        match self {
            Self::Application { .. } => "same-origin",
            Self::SandboxLocal | Self::SandboxHttps => "no-referrer",
        }
    }
    /// Builds a bounded response policy from an already selected immutable asset.
    pub fn content_security_policy(&self, origin: &str) -> Result<String> {
        let url = url::Url::parse(origin)
            .map_err(|_| ApiError::Invalid("invalid document origin".into()))?;
        if origin.len() > 2048
            || url.host_str().is_none()
            || !url.username().is_empty()
            || url.password().is_some()
            || !matches!(url.path(), "" | "/")
            || url.query().is_some()
            || url.fragment().is_some()
            || origin
                .chars()
                .any(|c| c.is_whitespace() || matches!(c, '\'' | '"' | ';'))
        {
            return Err(ApiError::Invalid("invalid document origin".into()));
        }
        match self {
            Self::Application { frame_paths } => {
                if frame_paths.len() > 32
                    || frame_paths.iter().any(|path| {
                        let name = path
                            .strip_prefix('/')
                            .unwrap_or("")
                            .strip_suffix('/')
                            .unwrap_or_else(|| path.strip_prefix('/').unwrap_or(""));
                        !path.starts_with('/')
                            || name.is_empty()
                            || matches!(name, "." | "..")
                            || path.len() > 256
                            || !name.bytes().all(|b| {
                                b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-')
                            })
                    })
                {
                    return Err(ApiError::Invalid("invalid framed asset paths".into()));
                }
                let frames = if frame_paths.is_empty() {
                    "'none'".into()
                } else {
                    frame_paths
                        .iter()
                        .map(|path| format!("{origin}{path}"))
                        .collect::<Vec<_>>()
                        .join(" ")
                };
                Ok(format!(
                    "default-src 'none'; script-src 'self' 'wasm-unsafe-eval'; style-src 'self'; connect-src 'self'; worker-src 'self'; img-src 'self' blob:; font-src 'self'; frame-src {frames}; base-uri 'none'; object-src 'none'; frame-ancestors 'none'; form-action 'none'"
                ))
            }
            Self::SandboxLocal | Self::SandboxHttps => {
                let https = if matches!(self, Self::SandboxHttps) {
                    " https:"
                } else {
                    ""
                };
                let connect = if matches!(self, Self::SandboxHttps) {
                    "https:"
                } else {
                    "'none'"
                };
                Ok(format!(
                    "sandbox allow-scripts; default-src 'none'; script-src 'unsafe-inline' blob:{https}; style-src 'unsafe-inline' blob:{https}; img-src blob:{https}; font-src blob: data:{https}; connect-src {connect}; worker-src 'none'; frame-src 'none'; base-uri 'none'; object-src 'none'; frame-ancestors {origin}; form-action 'none'"
                ))
            }
        }
    }
}

/// Immutable application bytes with their owning retention lease.
#[derive(Clone, Debug)]
pub struct HttpAsset {
    /// Registered representation; never inferred from an untrusted request.
    pub kind: AssetType,
    /// Response policy selected by the immutable asset provider.
    pub policy: DocumentPolicy,
    /// Provider-admitted immutable bytes shared through transport delivery.
    pub bytes: RetainedBytes,
}

/// Read-only exact bundle lookup; providers own input validation and retirement.
pub trait HttpAssets: fmt::Debug + Send + Sync + 'static {
    /// Returns one exact absolute URI path, or None for a missing asset.
    fn get(&self, path: &str) -> Result<Option<HttpAsset>>;
}

/// Nominal Local capability for explicitly composed application assets.
#[derive(Debug)]
pub struct HttpAssetsContract;
impl LocalContract for HttpAssetsContract {
    const KEY: &'static str = "rsi.api.http.assets";
    type Service = dyn HttpAssets;
}

pub(crate) fn response(
    asset: Option<HttpAsset>,
    headers: &HeaderMap,
    origin: &str,
) -> Result<Response<Body>> {
    if headers.contains_key("range") || headers.contains_key("content-encoding") {
        return Err(ApiError::Invalid(
            "asset requests do not accept ranges or encodings".into(),
        ));
    }
    let Some(asset) = asset else {
        return Ok(Response::builder()
            .status(404)
            .body(full(bytes::Bytes::new()))
            .expect("constant response"));
    };
    Ok(Response::builder()
        .status(200)
        .header("content-type", asset.kind.mime())
        .header("content-length", asset.bytes.len())
        .header(
            "content-security-policy",
            asset.policy.content_security_policy(origin)?,
        )
        .header("referrer-policy", asset.policy.referrer_policy())
        .header("x-content-type-options", "nosniff")
        .body(full(asset.bytes.into_bytes()))
        .expect("constant asset headers"))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn document_origins_and_frame_paths_cannot_inject_directives() {
        for origin in ["https://localhost:8443", "rsi://localhost"] {
            let policy = DocumentPolicy::Application {
                frame_paths: vec!["/preview-local.html".into(), "/downloads/".into()],
            };
            assert!(
                policy
                    .content_security_policy(origin)
                    .unwrap()
                    .contains(&format!("frame-src {origin}/preview-local.html"))
            );
            assert!(
                policy
                    .content_security_policy(origin)
                    .unwrap()
                    .contains(&format!("{origin}/downloads/"))
            );
            assert_eq!(policy.referrer_policy(), "same-origin");
            let local = DocumentPolicy::SandboxLocal
                .content_security_policy(origin)
                .unwrap();
            assert!(local.contains("sandbox allow-scripts;"));
            assert!(local.contains("connect-src 'none';"));
            assert!(!local.contains("allow-same-origin"));
        }
        for origin in [
            "https://user:secret@localhost",
            "https://localhost/path",
            "https://localhost;script-src *",
            "https://localhost/?x=1",
        ] {
            assert!(
                DocumentPolicy::default()
                    .content_security_policy(origin)
                    .is_err()
            );
        }
        for path in [
            "/",
            "/../",
            "/./",
            "//foreign",
            "/../x",
            "/x;script-src *",
            "/x?query",
        ] {
            assert!(
                DocumentPolicy::Application {
                    frame_paths: vec![path.into()]
                }
                .content_security_policy("https://localhost")
                .is_err()
            );
        }
    }
}
