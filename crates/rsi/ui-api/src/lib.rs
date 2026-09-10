//! Authenticated, owned UI presentations over the ordinary domain API registry.
#![forbid(unsafe_code)]
#![warn(missing_docs)]
#![allow(clippy::missing_errors_doc)]

mod binding;
mod client;
mod observe;
mod protocol;
mod state;
pub use binding::{UiBinding, UiBindingOwner, UiTargetBinder, UiTargetBinderContract};
pub use client::{UiClient, UiItem, UiObservation};
pub use protocol::{
    CatalogCursor, CatalogEntry, CatalogPage, CatalogRequest, ExportScope, Invoke, Item,
    MAXIMUM_ITEM_BYTES, Observe, Selection, Source, operations,
};

use async_trait::async_trait;
use rsi_api_protocol::{
    ApiContext, ApiError, ApiHandler, ApiMessage, ApiOutput, ApiRegistrar, ApiRegistrarContract,
    ApiRegistration, ApiResponseCapacity, Result, RetainedBytes,
};
use rsi_meta::{
    ActivationPlan, ConfigValue, Execution, MetaError, PluginFactory, PreparedActivation,
};
use serde::de::DeserializeOwned;
use state::State;
use std::sync::Arc;

/// Owns the API registrations, observers and target cleanup independently of listeners.
#[derive(Debug)]
pub struct UiApi {
    registrations: Vec<ApiRegistration>,
    state: Arc<State>,
}
impl UiApi {
    /// Registers operations using an explicit execution context and semantic binder.
    pub fn register(
        registrar: &dyn ApiRegistrar,
        execution: Execution,
        binder: Arc<dyn UiTargetBinder>,
    ) -> Result<Self> {
        let state = Arc::new(State::new(execution, binder));
        let mut registrations = Vec::new();
        for name in ["catalog", "observe", "invoke", "source"] {
            registrations.push(registrar.register(
                protocol::operation(name),
                Arc::new(Handler {
                    name,
                    state: state.clone(),
                }),
            )?);
        }
        Ok(Self {
            registrations,
            state,
        })
    }
    /// Fences all observers, drains admitted mutations, then joins target ownership.
    pub async fn close(mut self) -> Result<()> {
        self.state.stop.cancel();
        for registration in std::mem::take(&mut self.registrations) {
            registration.close().await;
        }
        self.state.tasks.close();
        self.state.tasks.wait().await;
        if self.state.failed.load(std::sync::atomic::Ordering::Acquire) {
            Err(ApiError::Backend("UI target cleanup failed".into()))
        } else {
            Ok(())
        }
    }
}
impl Drop for UiApi {
    fn drop(&mut self) {
        self.state.stop.cancel();
    }
}
#[derive(Debug)]
struct Handler {
    name: &'static str,
    state: Arc<State>,
}
#[async_trait]
impl ApiHandler for Handler {
    async fn invoke(
        &self,
        context: ApiContext,
        input: RetainedBytes,
        output: ApiResponseCapacity,
    ) -> Result<ApiOutput> {
        match self.name {
            "observe" => self.state.observe(context, decode(&input)?, output),
            "catalog" => {
                let scope = decode(&input)?;
                let state = self.state.clone();
                let token = state.tasks.token();
                let stop = state.stop.child_token();
                let _cancel = stop.clone().drop_guard();
                let task = self.state.execution.spawn(async move {
                    let _token = token;
                    state.catalog(context, scope, stop).await
                });
                let entries = task
                    .await
                    .map_err(|_| ApiError::Backend("UI catalog task failed".into()))??;
                reply(output, &entries)
            }
            "invoke" => {
                self.state.invoke(context, decode(&input)?).await?;
                reply(output, &true)
            }
            "source" => {
                let bytes = self.state.source(context, decode(&input)?).await?;
                let ApiResponseCapacity::Finite(mut capacity) = output else {
                    return Err(ApiError::Unavailable);
                };
                let binary = capacity
                    .split(bytes.len())?
                    .retain_vec(bytes.as_bytes().to_vec())?
                    .with_retention(bytes);
                let json = capacity.encode(&serde_json::json!({"bytes": binary.len()}))?;
                Ok(ApiOutput::Reply(ApiMessage {
                    json,
                    binary: Some(binary),
                }))
            }
            _ => Err(ApiError::Unavailable),
        }
    }
}
fn decode<T: DeserializeOwned>(input: &RetainedBytes) -> Result<T> {
    serde_json::from_slice(input.as_bytes())
        .map_err(|_| ApiError::Invalid("invalid UI request".into()))
}
fn reply(output: ApiResponseCapacity, value: &impl serde::Serialize) -> Result<ApiOutput> {
    let ApiResponseCapacity::Finite(capacity) = output else {
        return Err(ApiError::Unavailable);
    };
    Ok(ApiOutput::Reply(ApiMessage {
        json: capacity.encode(value)?,
        binary: None,
    }))
}
pub(crate) fn ui_error(error: &rsi_ui::UiError) -> ApiError {
    match error {
        rsi_ui::UiError::Capacity => ApiError::Capacity,
        rsi_ui::UiError::Retired => ApiError::Unavailable,
        rsi_ui::UiError::Action(_) => ApiError::OutcomeUnknown,
        _ => ApiError::Invalid("invalid UI presentation request".into()),
    }
}
/// Ordinary API plugin; its target binder is explicitly selected by the product.
#[derive(Clone, Debug, Default)]
pub struct UiApiFactory;
#[async_trait]
impl PluginFactory for UiApiFactory {
    fn prepare(&self, config: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        if !config.is_null() {
            return Err(MetaError::InvalidInput(
                "UI API expects null configuration".into(),
            ));
        }
        Ok(PreparedActivation::new(ConfigValue::Null)
            .requiring_local::<ApiRegistrarContract>()
            .requiring_local::<UiTargetBinderContract>())
    }
    async fn activate(&self, plan: ActivationPlan) -> rsi_meta::Result<()> {
        let api = UiApi::register(
            plan.local::<ApiRegistrarContract>()?.as_ref(),
            plan.context().runtime().execution().clone(),
            plan.local::<UiTargetBinderContract>()?,
        )
        .map_err(|e| MetaError::Activation(e.to_string()))?;
        plan.defer(
            "retire UI API",
            Box::new(move || {
                Box::pin(async move { api.close().await.map_err(|error| error.to_string()) })
            }),
        )
    }
}
