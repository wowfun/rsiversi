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

/// Immutable application bytes with their owning retention lease.
#[derive(Clone, Debug)]
pub struct HttpAsset {
    /// Registered representation; never inferred from an untrusted request.
    pub kind: AssetType,
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

pub(crate) fn response(asset: Option<HttpAsset>, headers: &HeaderMap) -> Result<Response<Body>> {
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
        .header("content-security-policy", "default-src 'none'; script-src 'self' 'wasm-unsafe-eval'; style-src 'self'; connect-src 'self'; worker-src 'self'; img-src 'self' blob:; font-src 'self'; base-uri 'none'; object-src 'none'; frame-ancestors 'none'; form-action 'none'")
        .header("referrer-policy", "same-origin")
        .body(full(asset.bytes.into_bytes())).expect("constant asset headers"))
}
