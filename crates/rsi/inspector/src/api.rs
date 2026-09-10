use crate::{
    FactoryDeclaration, InspectorSource, MAXIMUM_RESPONSE_BYTES, PageRequest, RuntimeRequest,
    projection,
};
use rsi_api_protocol::{
    ApiClient, ApiError, ApiRegistrar, ApiRegistration, OperationAccess, OperationClass,
    OperationEffect, OperationId, OperationSpec, RequestEncoding, Result, call_json, json_handler,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::sync::Arc;

const NAMES: [&str; 4] = ["runtime", "profile", "factories", "native"];
fn operation(name: &str) -> OperationSpec {
    OperationSpec {
        id: OperationId::new("inspector", name, 1).expect("constant Inspector operation"),
        access: OperationAccess::Local,
        class: OperationClass::Data,
        effect: OperationEffect::Read,
        encoding: RequestEncoding::Json,
        maximum_request_bytes: 1024,
        maximum_response_bytes: MAXIMUM_RESPONSE_BYTES,
    }
}
#[derive(Deserialize, Serialize)]
enum Never {}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Empty {}

/// Owns read-only registrations independently of listeners and clients.
#[derive(Debug)]
pub struct InspectorApi(Vec<ApiRegistration>);
impl InspectorApi {
    /// Registers finite local-only operations over an explicitly supplied source.
    pub fn register(
        registrar: &dyn ApiRegistrar,
        source: Arc<dyn InspectorSource>,
    ) -> Result<Self> {
        let runtime = source.clone();
        let a = registrar.register(
            operation("runtime"),
            json_handler(move |_, request: RuntimeRequest| {
                let source = runtime.clone();
                async move {
                    let value = source.runtime(request.validate()?)?;
                    Ok::<_, ApiError>(Ok::<_, Never>(projection::runtime(value)))
                }
            }),
        )?;
        let profile = source.clone();
        let b = registrar.register(
            operation("profile"),
            json_handler(move |_, request: PageRequest| {
                let source = profile.clone();
                async move {
                    request.validate()?;
                    let (status, snapshot) = source.profile()?;
                    Ok::<_, ApiError>(Ok::<_, Never>(projection::profile(
                        &status, &snapshot, request,
                    )))
                }
            }),
        )?;
        let factories = source.clone();
        let c = registrar.register(operation("factories"), json_handler(move |_, request: PageRequest| {
            let source = factories.clone();
            async move {
                request.validate()?;
                let values = source.factories();
                let rows: Vec<&FactoryDeclaration> = values.iter().skip(request.offset).take(request.limit).collect();
                let next = request.offset.saturating_add(rows.len());
                Ok::<_, ApiError>(Ok::<_, Never>(json!({ "total": values.len(), "next_offset": (next < values.len()).then_some(next), "factories": rows })))
            }
        }))?;
        let d = registrar.register(
            operation("native"),
            json_handler(move |_, _: Empty| {
                let source = source.clone();
                async move { Ok::<_, ApiError>(Ok::<_, Never>(source.native()?)) }
            }),
        )?;
        Ok(Self(vec![a, b, c, d]))
    }
    /// Withdraws discovery/invocation and drains already admitted reads.
    pub async fn close(self) {
        for registration in self.0 {
            registration.close().await;
        }
    }
}

/// Finite display-document client; returned JSON conveys no callable authority.
#[derive(Debug)]
pub struct InspectorClient(Arc<dyn ApiClient>);
impl InspectorClient {
    /// Requires the complete exact local Inspector descriptor set.
    pub fn new(api: Arc<dyn ApiClient>) -> Result<Self> {
        if !NAMES
            .iter()
            .all(|name| api.operations().contains(&operation(name)))
        {
            return Err(ApiError::Unavailable);
        }
        Ok(Self(api))
    }
    async fn call<I: Serialize + Sync>(&self, name: &str, request: &I) -> Result<Value> {
        match call_json::<_, Value, Never>(self.0.as_ref(), &operation(name), request).await? {
            Ok(value) if value.is_object() => Ok(value),
            Ok(_) => Err(ApiError::Invalid("invalid Inspector document".into())),
            Err(never) => match never {},
        }
    }
    /// Reads one actual Runtime membership page.
    pub async fn runtime(&self, request: &RuntimeRequest) -> Result<Value> {
        request.validate()?;
        self.call("runtime", request).await
    }
    /// Reads one redacted desired Profile tree page.
    pub async fn profile(&self, request: &PageRequest) -> Result<Value> {
        request.validate()?;
        self.call("profile", request).await
    }
    /// Reads frozen executable declarations without preparing them.
    pub async fn factories(&self, request: &PageRequest) -> Result<Value> {
        request.validate()?;
        self.call("factories", request).await
    }
    /// Reads native selection, resources and retained failure state.
    pub async fn native(&self) -> Result<Value> {
        self.call("native", &Empty {}).await
    }
}
