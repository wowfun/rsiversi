//! Explicit local native staging control, independent of the product Loader owner.
use async_trait::async_trait;
use rsi_api_protocol::{
    ApiClient, ApiError, ApiRegistrar, ApiRegistration, OperationAccess, OperationClass,
    OperationEffect, OperationId, OperationSpec, RequestEncoding, Result, call_json, json_handler,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

/// A successful staging observation, not a Session or Runtime apply receipt.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RefreshReceipt {
    /// Validated desired source revision, represented exactly in JSON.
    #[serde(with = "revision")]
    pub source_revision: u64,
    /// Whether the executable Agent snapshot changed.
    pub changed: bool,
    /// Number of selected native factories.
    pub selected: usize,
}
mod revision {
    use serde::{Deserialize, Deserializer, Serializer, de::Error as _};
    #[expect(
        clippy::trivially_copy_pass_by_ref,
        reason = "Serde with passes a borrowed field"
    )]
    pub fn serialize<S: Serializer>(value: &u64, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&value.to_string())
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<u64, D::Error> {
        let value = String::deserialize(deserializer)?;
        let parsed: u64 = value.parse().map_err(D::Error::custom)?;
        if value != parsed.to_string() {
            return Err(D::Error::custom("noncanonical source revision"));
        }
        Ok(parsed)
    }
}
/// Categorical failures deliberately omit native diagnostics and local paths.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, thiserror::Error)]
#[serde(rename_all = "snake_case")]
pub enum RefreshFailure {
    /// Source metadata or artifact storage failed validation.
    #[error("native source unavailable")]
    Source,
    /// Loader admission or a native callback rejected the candidate.
    #[error("native load rejected")]
    Load,
    /// The complete proposed Agent declaration selection is invalid.
    #[error("invalid native selection")]
    Selection,
    /// The bounded worker queue cannot admit this request.
    #[error("native refresh busy")]
    Busy,
    /// The ordinary manager has closed new staging.
    #[error("native refresh closed")]
    Closed,
    /// Failure retention closed the sole Loader's admission.
    #[error("native resources retained; process recovery required")]
    Retained,
}
/// The operation's explicit product-owned staging capability.
#[async_trait]
pub trait NativeAddonAdministration: std::fmt::Debug + Send + Sync + 'static {
    /// Makes one explicit attempt, without automatic replay on delivery failure.
    async fn refresh(&self) -> std::result::Result<RefreshReceipt, RefreshFailure>;
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Empty {}
fn operation() -> OperationSpec {
    OperationSpec {
        id: OperationId::new("native-addons", "refresh", 1).expect("constant native operation"),
        access: OperationAccess::Local,
        class: OperationClass::Data,
        effect: OperationEffect::Mutation,
        encoding: RequestEncoding::Json,
        maximum_request_bytes: 1024,
        maximum_response_bytes: 4096,
    }
}
/// Owns local registration admission and drain, independently of transport listeners.
#[derive(Debug)]
pub struct NativeAddonsApi(ApiRegistration);
impl NativeAddonsApi {
    /// Registers the operation over the one explicitly supplied manager.
    ///
    /// # Errors
    /// Returns registration conflict, capacity or shutdown failures.
    pub fn register(
        registrar: &dyn ApiRegistrar,
        administration: Arc<dyn NativeAddonAdministration>,
    ) -> Result<Self> {
        registrar
            .register(
                operation(),
                json_handler(move |_, _: Empty| {
                    let administration = administration.clone();
                    async move { Ok::<_, ApiError>(administration.refresh().await) }
                }),
            )
            .map(Self)
    }
    /// Withdraws discovery and joins admitted requests; cancel the adapter's Control waits first.
    pub async fn close(self) {
        self.0.close().await;
    }
}
/// Negotiated local client. Transport uncertainty is returned without retry.
#[derive(Debug)]
pub struct NativeAddonsClient(Arc<dyn ApiClient>);
impl NativeAddonsClient {
    /// Requires the exact local mutation descriptor.
    ///
    /// # Errors
    /// Returns unavailable when the negotiated descriptor is absent or differs.
    pub fn new(api: Arc<dyn ApiClient>) -> Result<Self> {
        if !api.operations().contains(&operation()) {
            return Err(ApiError::Unavailable);
        }
        Ok(Self(api))
    }
    /// Attempts the current selection once and returns its typed staging outcome.
    ///
    /// # Errors
    /// Returns transport, admission or wire validation failures without replay.
    /// The inner result carries the explicit staging failure.
    pub async fn refresh(&self) -> Result<std::result::Result<RefreshReceipt, RefreshFailure>> {
        call_json(self.0.as_ref(), &operation(), &Empty {}).await
    }
}
