//! Session and Tool UI contributions over the exact surface controller.
#![forbid(unsafe_code)]
#![warn(missing_docs)]
#![allow(clippy::missing_errors_doc)]

use async_trait::async_trait;
use futures_util::future::BoxFuture;
use rsi_client::{SessionController, SessionControllerContract};
use rsi_conversation::{FieldWindow, SourceRef};
use rsi_meta::{
    ActivationPlan, ConfigValue, Context, MetaError, PluginFactory, PreparedActivation,
};
use rsi_ui::{
    ActionContribution, ActionInput, ActionTarget, BlockInput, BlockRenderer,
    BlockRendererContribution, Contributions, Result, SurfaceContribution, SurfaceRenderer,
    TargetKind, UiAction, UiContract, UiElement, UiError, UiTargetContract, UiView,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

/// Raw source window preference for contributed detail cards.
pub const SOURCE_PAGE_BYTES: usize = 16 * 1024;
fn meta(error: impl std::fmt::Display) -> MetaError {
    MetaError::Activation(error.to_string())
}
fn controller(context: &Context) -> Result<Arc<SessionController>> {
    context
        .lookup_local::<SessionControllerContract>()
        .ok_or(UiError::Retired)
}
fn no_config(desired: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
    if !desired.is_null() {
        return Err(MetaError::InvalidInput(
            "Session UI configuration must be null".into(),
        ));
    }
    Ok(PreparedActivation::new(ConfigValue::Null))
}
/// Exact UI target whose lifetime depends on the actual Session controller.
#[derive(Debug, Clone, Default)]
pub struct SessionUiTargetFactory;
#[async_trait]
impl PluginFactory for SessionUiTargetFactory {
    fn prepare(&self, desired: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        Ok(no_config(desired)?
            .requiring_local::<UiContract>()
            .requiring_local::<SessionControllerContract>())
    }
    async fn activate(&self, plan: ActivationPlan) -> rsi_meta::Result<()> {
        let (target, lease) = plan
            .local::<UiContract>()?
            .register_target(&plan, TargetKind::Surface)
            .map_err(meta)?;
        let supply = plan.context().provide_local::<UiTargetContract>(target)?;
        plan.defer(
            "withdraw Session UI target",
            Box::new(move || {
                Box::pin(async move {
                    drop(supply);
                    let report = lease.dispose().await;
                    if report.is_clean() {
                        Ok(())
                    } else {
                        Err("Session UI target cleanup failed".into())
                    }
                })
            }),
        )
    }
}
/// First-party Session surface and Tool block/source contributions.
#[derive(Debug, Clone, Default)]
pub struct SessionUiFactory;
#[async_trait]
impl PluginFactory for SessionUiFactory {
    fn prepare(&self, desired: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        Ok(no_config(desired)?.requiring_local::<UiContract>())
    }
    async fn activate(&self, plan: ActivationPlan) -> rsi_meta::Result<()> {
        let lease = plan
            .local::<UiContract>()?
            .register(
                &plan,
                Contributions {
                    name: "rsi.session.inspection".into(),
                    surfaces: vec![SurfaceContribution {
                        name: "session".into(),
                        title: "Session details".into(),
                        target: TargetKind::Surface,
                        renderer: Arc::new(SessionCard),
                    }],
                    actions: vec![ActionContribution {
                        name: "source".into(),
                        target: TargetKind::Surface,
                        handler: Arc::new(ReadSource),
                    }],
                    renderers: vec![BlockRendererContribution {
                        name: "tool".into(),
                        target: TargetKind::Surface,
                        renderer: Arc::new(ToolCard),
                    }],
                },
            )
            .map_err(meta)?;
        plan.defer(
            "release Session UI contributions",
            Box::new(move || {
                Box::pin(async move {
                    let report = lease.dispose().await;
                    if report.is_clean() {
                        Ok(())
                    } else {
                        Err("Session UI contribution cleanup failed".into())
                    }
                })
            }),
        )
    }
}
#[derive(Debug)]
struct SessionCard;
impl SurfaceRenderer for SessionCard {
    fn render(&self, target: &Context) -> Result<UiView> {
        let controller = controller(target)?;
        Ok(UiView {
            title: "Session details".into(),
            elements: vec![
                UiElement::Field {
                    label: "Session".into(),
                    value: controller.session_id().to_string(),
                },
                UiElement::Text {
                    text: "Open a card to read its complete arguments, results or rejection."
                        .into(),
                },
            ],
        })
    }
}
#[derive(Debug)]
struct ToolCard;
impl BlockRenderer for ToolCard {
    fn render(&self, target: &Context, block: &BlockInput<'_>) -> Result<Option<UiView>> {
        let controller = controller(target)?;
        let Some(tool) = block.tool else {
            let window =
                FieldWindow::text(block.text, 0, SOURCE_PAGE_BYTES).expect("valid card window");
            let mut elements = vec![UiElement::Code { text: window.text }];
            if window.more {
                elements.push(UiElement::Text {
                    text: "Preview shortened. Open a source below for complete pages.".into(),
                });
            }
            for source in block.sources.iter().take(32) {
                elements.push(source_button(
                    &format!("Fact {} · {}", source.seq, source.field),
                    source,
                    0,
                ));
            }
            if block.sources.len() > 32 {
                elements.push(UiElement::Text { text: "This card lists the first 32 references. The block's source list contains all retained references.".into() });
            }
            return Ok(Some(UiView {
                title: "Block details".into(),
                elements,
            }));
        };
        let mut elements = vec![
            UiElement::Field {
                label: "Session".into(),
                value: controller.session_id().to_string(),
            },
            UiElement::Field {
                label: "Intent".into(),
                value: if tool.intent_present {
                    "Loaded"
                } else {
                    "Not present in this history window"
                }
                .into(),
            },
        ];
        for (label, source) in [
            ("Arguments", tool.arguments),
            ("Result", tool.result),
            ("Rejection", tool.rejection),
        ] {
            if let Some(source) = source {
                elements.push(source_button(label, source, 0));
            }
        }
        Ok(Some(UiView {
            title: FieldWindow::text(&tool.title(), 0, 256)
                .expect("valid title bound")
                .text,
            elements,
        }))
    }
}
#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct SourcePage {
    source: SourceRef,
    start: usize,
}
fn source_button(label: &str, source: SourceRef, start: usize) -> UiElement {
    UiElement::Button {
        action: "source".into(),
        label: label.into(),
        value: serde_json::to_value(SourcePage { source, start }).expect("closed source payload"),
    }
}
#[derive(Debug)]
struct ReadSource;
impl UiAction for ReadSource {
    fn invoke(
        &self,
        target: ActionTarget,
        input: ActionInput,
    ) -> BoxFuture<'static, Result<UiView>> {
        Box::pin(async move {
            if !input.fields.is_empty() {
                return Err(UiError::Invalid(
                    "source reading accepts no form fields".into(),
                ));
            }
            let request: SourcePage = serde_json::from_value(input.value)
                .map_err(|error| UiError::Invalid(error.to_string()))?;
            let controller = controller(target.context())?;
            let stop = CancellationToken::new();
            let _cancel = stop.clone().drop_guard();
            let window = tokio::select! { biased;
                () = target.cancelled() => return Err(UiError::Retired),
                () = target.view_closed() => return Err(UiError::Retired),
                result = controller.source_window(request.source, request.start, SOURCE_PAGE_BYTES, stop) => result.map_err(|error| UiError::Action(error.to_string()))?,
            };
            Ok(source_view(request.source, window))
        })
    }
}
fn source_view(source: SourceRef, window: FieldWindow) -> UiView {
    let mut elements = vec![
        UiElement::Field {
            label: "Raw UTF-8 bytes".into(),
            value: format!(
                "{}–{}{}",
                window.start,
                window.end,
                if window.more {
                    " · more available"
                } else {
                    " · end"
                }
            ),
        },
        UiElement::Code { text: window.text },
    ];
    if window.start > 0 {
        elements.push(source_button(
            "Previous page",
            source,
            window.start.saturating_sub(SOURCE_PAGE_BYTES),
        ));
    }
    if window.more {
        elements.push(source_button("Next page", source, window.end));
    }
    UiView {
        title: format!("Fact {} · {}", source.seq, source.field),
        elements,
    }
}
