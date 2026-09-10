//! Explicit Portable-to-Local UI model adapter under ordinary Meta ownership.
#![forbid(unsafe_code)]
#![warn(missing_docs)]
#![allow(clippy::missing_errors_doc)]

use async_trait::async_trait;
use futures_util::future::BoxFuture;
use rsi_api_protocol::{ByteBudget, RetainedBytes};
use rsi_meta::{
    ActivationPlan, Capability, ConfigValue, Context, ContractVersion, Message, MetaError,
    PluginFactory, PreparedActivation, Requirement,
};
use rsi_ui::{
    ActionContribution, ActionInput, ActionTarget, Contributions, PresentationIdentity,
    SurfaceContribution, SurfaceRenderer, UiAction, UiContract, UiError, UiModel, UiView,
};
use rsi_ui_protocol::portable::{self, Description, Request};
use serde::Deserialize;
use std::sync::{Arc, LazyLock};
mod scope;

static WIRE: LazyLock<ByteBudget> = LazyLock::new(ByteBudget::default);

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Config {
    service: String,
    #[serde(default)]
    business_api: bool,
}

/// Imports one selected Portable UI source into the actual Local UI registry.
#[derive(Clone, Debug, Default)]
pub struct PortableUiFactory;
#[async_trait]
impl PluginFactory for PortableUiFactory {
    fn prepare(&self, desired: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        let config: Config = serde_json::from_value(desired.clone()).map_err(|_| invalid())?;
        if !rsi_ui_protocol::name_valid(&config.service) {
            return Err(invalid());
        }
        let requirement = Requirement::new(
            config.service.clone(),
            portable::CONTRACT,
            ContractVersion(portable::VERSION),
        );
        Ok(PreparedActivation::with_state(desired.clone(), config, 256)
            .requiring(requirement)
            .requiring_local::<UiContract>())
    }
    async fn activate(&self, mut plan: ActivationPlan) -> rsi_meta::Result<()> {
        let config = plan.take_state::<Config>()?;
        let adapter = Arc::new(Adapter {
            capability: plan.inject(&config.service).ok_or_else(invalid)?.clone(),
            service: config.service,
            business_api: config.business_api,
            grant: None,
            scope: None,
            stop: tokio_util::sync::CancellationToken::new(),
        });
        let bytes = adapter
            .exchange(Request::Describe {}, portable::MAXIMUM_PACKET_BYTES)
            .await
            .map_err(activation)?;
        let description: Description =
            serde_json::from_slice(bytes.as_bytes()).map_err(|_| invalid())?;
        if description.surfaces.len() > rsi_ui::MAXIMUM_CONTRIBUTIONS
            || description.actions.len() > rsi_ui::MAXIMUM_CONTRIBUTIONS
        {
            return Err(invalid());
        }
        let contributions = Contributions {
            name: description.name,
            surfaces: description
                .surfaces
                .into_iter()
                .map(|surface| SurfaceContribution {
                    name: surface.name,
                    title: surface.title,
                    target: surface.target,
                    renderer: adapter.clone(),
                })
                .collect(),
            actions: description
                .actions
                .into_iter()
                .map(|action| ActionContribution {
                    handler: Arc::new(Action {
                        adapter: adapter.clone(),
                        name: action.name.clone(),
                    }),
                    name: action.name,
                    target: action.target,
                })
                .collect(),
            renderers: vec![],
        };
        let lease = plan
            .local::<UiContract>()?
            .register(&plan, contributions)
            .map_err(activation)?;
        plan.defer(
            "withdraw Portable UI contribution",
            Box::new(move || {
                Box::pin(async move {
                    let report = lease.dispose().await;
                    if report.is_clean() {
                        Ok(())
                    } else {
                        Err("Portable UI cleanup failed".into())
                    }
                })
            }),
        )?;
        Ok(())
    }
}
#[derive(Clone, Debug)]
struct Adapter {
    capability: Capability,
    service: String,
    business_api: bool,
    grant: Option<Capability>,
    scope: Option<rsi_ui::ExportScope>,
    stop: tokio_util::sync::CancellationToken,
}
impl Adapter {
    fn bound(&self, context: &Context) -> rsi_ui::Result<Self> {
        if self.business_api {
            Ok(context
                .lookup_local::<scope::BoundContract>()
                .ok_or(UiError::Retired)?
                .adapter
                .clone())
        } else {
            Ok(self.clone())
        }
    }
    async fn exchange(&self, request: Request, maximum: usize) -> rsi_ui::Result<RetainedBytes> {
        if self.stop.is_cancelled() {
            return Err(UiError::Retired);
        }
        let request = WIRE
            .encode(&request, portable::MAXIMUM_PACKET_BYTES)
            .map_err(|_| protocol())?;
        // The transient Message copy is reserved until Meta accepts its queue ownership.
        let outbound = WIRE.reserve(request.len()).map_err(|_| UiError::Capacity)?;
        let incoming = WIRE.reserve(maximum).map_err(|_| UiError::Capacity)?;
        let mut call = self.capability.open().map_err(|_| protocol())?;
        let result = async {
            call.send(Message::from_parts(
                request.as_bytes().to_vec(),
                self.grant.iter().cloned().collect::<Vec<_>>(),
            ))
            .await
            .map_err(|_| protocol())?;
            drop(outbound);
            call.finish();
            let message = call
                .recv()
                .await
                .map_err(|_| protocol())?
                .ok_or_else(protocol)?;
            if !message.capabilities().is_empty() || message.as_bytes().len() > maximum {
                return Err(protocol());
            }
            let (bytes, _) = message.into_parts();
            let retained = incoming.retain_vec(bytes).map_err(|_| protocol())?;
            if call.recv().await.map_err(|_| protocol())?.is_some() {
                return Err(protocol());
            }
            Ok(retained)
        }
        .await;
        if result.is_err() {
            call.cancel();
            while matches!(call.recv().await, Ok(Some(_))) {}
        }
        result
    }
    async fn model_request(&self, request: Request) -> rsi_ui::Result<UiModel> {
        let bytes = self
            .exchange(request, portable::MAXIMUM_PACKET_BYTES)
            .await?;
        let model: UiModel = serde_json::from_slice(bytes.as_bytes()).map_err(|_| protocol())?;
        model.validate()?;
        Ok(model)
    }
}
impl SurfaceRenderer for Adapter {
    fn bind(
        &self,
        target: Context,
        _: PresentationIdentity,
        stop: tokio_util::sync::CancellationToken,
    ) -> BoxFuture<'_, rsi_ui::Result<Option<rsi_ui::PresentationBinding>>> {
        Box::pin(async move {
            if self.business_api {
                scope::bind(target, self.service.clone(), stop).await
            } else {
                Ok(None)
            }
        })
    }
    fn model_in(
        &self,
        target: Context,
        presentation: PresentationIdentity,
    ) -> BoxFuture<'_, rsi_ui::Result<UiModel>> {
        Box::pin(async move {
            let adapter = self.bound(&target)?;
            adapter
                .model_request(Request::Snapshot {
                    presentation,
                    scope: adapter.scope.clone(),
                })
                .await
        })
    }
    fn source(
        &self,
        target: ActionTarget,
        name: String,
        offset: u64,
        maximum: usize,
    ) -> BoxFuture<'static, rsi_ui::Result<Vec<u8>>> {
        let presentation = target.presentation().cloned();
        let adapter = self.bound(target.context());
        Box::pin(async move {
            let adapter = adapter?;
            let bytes = adapter
                .exchange(
                    Request::Source {
                        scope: adapter.scope.clone(),
                        presentation: presentation.ok_or_else(protocol)?,
                        name,
                        offset,
                        maximum,
                    },
                    maximum,
                )
                .await?;
            Ok(bytes.as_bytes().to_vec())
        })
    }
}
#[derive(Debug)]
struct Action {
    adapter: Arc<Adapter>,
    name: String,
}
impl UiAction for Action {
    fn invoke(
        &self,
        _: ActionTarget,
        _: ActionInput,
    ) -> BoxFuture<'static, rsi_ui::Result<UiView>> {
        Box::pin(async {
            Err(UiError::Invalid(
                "Portable UI actions require a presentation lease".into(),
            ))
        })
    }
    fn invoke_model(
        &self,
        target: ActionTarget,
        input: ActionInput,
    ) -> BoxFuture<'static, rsi_ui::Result<UiModel>> {
        let adapter = self.adapter.bound(target.context());
        let action = self.name.clone();
        let presentation = target.presentation().cloned();
        Box::pin(async move {
            let adapter = adapter?;
            adapter
                .model_request(Request::Invoke {
                    scope: adapter.scope.clone(),
                    presentation: presentation.ok_or_else(protocol)?,
                    action,
                    input,
                })
                .await
        })
    }
}
fn invalid() -> MetaError {
    MetaError::InvalidInput("invalid Portable UI configuration or declaration".into())
}
fn activation(error: impl std::fmt::Display) -> MetaError {
    MetaError::Activation(error.to_string())
}
fn protocol() -> UiError {
    UiError::Action(
        "Portable UI exchange failed; dispatched action effects may be unresolved".into(),
    )
}
