use crate::{
    BrowserBinding, SessionAuthority, SessionBrowserContract, SessionOperation, SessionPolicy,
    SessionResult,
};
use async_trait::async_trait;
use futures_util::future::BoxFuture;
use rsi_meta::{
    ActivationPlan, ConfigValue, Context, MetaError, PluginFactory, PreparedActivation,
};
use rsi_ui::{
    ActionContribution, ActionInput, ActionTarget, Contributions, ModelSource, SurfaceContribution,
    SurfaceRenderer, TargetKind, UiAction, UiContract, UiElement, UiError, UiModel, UiView,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::sync::Arc;
use tokio_util::sync::CancellationToken;
fn error(e: impl std::fmt::Display) -> UiError {
    let mut detail = e.to_string();
    detail.truncate(detail.floor_char_boundary(4096));
    UiError::Action(detail)
}

#[cfg(test)]
mod diagnostic_tests {
    #[test]
    fn page_derived_action_diagnostics_fit_a_utf8_byte_bound() {
        let super::UiError::Action(detail) = super::error("界".repeat(4000)) else {
            unreachable!()
        };
        assert!(detail.len() <= 4096);
        assert!(detail.ends_with('界'));
    }
}
#[derive(Clone, Debug, Default)]
pub struct SessionBrowserUiFactory;
#[async_trait]
impl PluginFactory for SessionBrowserUiFactory {
    fn prepare(&self, config: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        if !config.is_null() {
            return Err(MetaError::InvalidInput(
                "browser UI config must be null".into(),
            ));
        }
        Ok(PreparedActivation::new(config.clone())
            .requiring_local::<UiContract>()
            .requiring_local::<SessionBrowserContract>())
    }
    async fn activate(&self, plan: ActivationPlan) -> rsi_meta::Result<()> {
        let lease = plan
            .local::<UiContract>()?
            .register(
                &plan,
                Contributions {
                    name: "rsi.browser".into(),
                    surfaces: vec![SurfaceContribution {
                        name: "session".into(),
                        title: "Session browser".into(),
                        target: TargetKind::Surface,
                        renderer: Arc::new(Surface),
                    }],
                    actions: vec![ActionContribution {
                        name: "operate".into(),
                        target: TargetKind::Surface,
                        handler: Arc::new(Operate),
                    }],
                    renderers: vec![],
                },
            )
            .map_err(|e| MetaError::Activation(e.to_string()))?;
        plan.defer(
            "withdraw Session browser UI",
            Box::new(move || {
                Box::pin(async move {
                    let report = lease.dispose().await;
                    if report.is_clean() {
                        Ok(())
                    } else {
                        Err("browser UI cleanup failed".into())
                    }
                })
            }),
        )
    }
}
async fn authority(context: &Context) -> rsi_ui::Result<SessionAuthority> {
    let controller = context
        .lookup_local::<rsi_client::SessionControllerContract>()
        .ok_or(UiError::Retired)?;
    let source = Arc::new(
        context
            .lookup_local::<rsi_session_protocol::SessionSourceContract>()
            .ok_or(UiError::Retired)?
            .acquire()
            .await
            .map_err(error)?,
    );
    if source.header().session_id() != controller.session_id() {
        return Err(UiError::Retired);
    }
    Ok(SessionAuthority::Human(source))
}
#[derive(Debug)]
struct Surface;
impl SurfaceRenderer for Surface {
    fn model(&self, target: Context) -> BoxFuture<'_, rsi_ui::Result<UiModel>> {
        Box::pin(async move {
            let owner = target
                .lookup_local::<SessionBrowserContract>()
                .ok_or(UiError::Retired)?;
            let result = owner
                .call(
                    authority(&target).await?,
                    SessionOperation::Status {},
                    CancellationToken::new(),
                )
                .await
                .map_err(error)?;
            model(&result)
        })
    }
    fn source(
        &self,
        target: ActionTarget,
        name: String,
        offset: u64,
        maximum: usize,
    ) -> BoxFuture<'static, rsi_ui::Result<Vec<u8>>> {
        Box::pin(async move {
            let owner = target
                .context()
                .lookup_local::<SessionBrowserContract>()
                .ok_or(UiError::Retired)?;
            let authority = authority(target.context()).await?;
            let result = owner
                .call(
                    authority.clone(),
                    SessionOperation::Status {},
                    CancellationToken::new(),
                )
                .await
                .map_err(error)?;
            let binding = result.binding.ok_or(UiError::Retired)?;
            let media = result.screenshot.ok_or(UiError::Retired)?;
            if name != format!("{}.{}.png", binding.browser_id, media.id) {
                return Err(UiError::Retired);
            }
            owner
                .screenshot_bytes(&authority, &binding, &media, offset, maximum)
                .await
                .map_err(error)
        })
    }
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Action {
    operation: String,
    binding: Option<BrowserBinding>,
    document_version: Option<String>,
    observation_id: Option<String>,
}
fn button(
    label: &str,
    operation: &str,
    binding: Option<&BrowserBinding>,
    snapshot: &Value,
) -> UiElement {
    UiElement::Button {
        action: "operate".into(),
        label: label.into(),
        value: json!({"operation":operation,"binding":binding,"document_version":snapshot.get("document_version"),"observation_id":snapshot.get("observation_id")}),
    }
}
fn input(name: &str, label: &str, value: String) -> UiElement {
    UiElement::Input {
        name: name.into(),
        label: label.into(),
        value,
        multiline: false,
    }
}
fn view(result: &SessionResult) -> UiView {
    let snapshot = &result.result["snapshot"];
    let mut elements = vec![
        UiElement::Field {
            label: "Browser".into(),
            value: format!(
                "{} · imported {} / 33554432 bytes",
                result.state, result.imported_bytes
            ),
        },
        input(
            "url",
            "Page URL",
            snapshot["url"].as_str().unwrap_or_default().into(),
        ),
        input("node", "Node ID", String::new()),
        input("text", "Text to fill", String::new()),
    ];
    if result.binding.is_none() {
        elements.push(button(
            "Open public HTTPS browser",
            "open_public",
            None,
            snapshot,
        ));
        elements.push(button(
            "Approve and open this exact local origin",
            "open_local",
            None,
            snapshot,
        ));
    } else {
        for (label, op) in [
            ("Read current state", "status"),
            ("Navigate", "navigate"),
            ("Observe page", "observe"),
            ("Click node", "click"),
            ("Fill node", "fill"),
            ("Scroll up", "up"),
            ("Scroll down", "down"),
            ("Capture screenshot", "screenshot"),
            ("Close browser", "close"),
        ] {
            elements.push(button(label, op, result.binding.as_ref(), snapshot));
        }
    }
    if !snapshot.is_null() {
        let mut code = UiElement::Code {
            text: serde_json::to_string_pretty(snapshot).expect("bounded structure"),
        };
        if serde_json::to_vec(&code).expect("bounded code").len() > 96 * 1024 {
            code = UiElement::Code {
                text: serde_json::to_string(snapshot).expect("bounded structure"),
            };
        }
        elements.push(code);
    }
    if result.result["status"] != "completed" {
        elements.push(UiElement::Text {
            text: {
                let mut diagnostic = result.result.clone();
                diagnostic
                    .as_object_mut()
                    .expect("typed result")
                    .remove("snapshot");
                serde_json::to_string(&diagnostic).expect("bounded status")
            },
        });
    }
    UiView {
        title: "Session browser".into(),
        elements,
    }
}
fn model(result: &SessionResult) -> rsi_ui::Result<UiModel> {
    let mut model = UiModel::standard(view(result))?;
    if let (Some(binding), Some(media)) = (&result.binding, &result.screenshot) {
        let name = format!("{}.{}.png", binding.browser_id, media.id);
        model.sources.push(ModelSource {
            name: name.clone(),
            title: "Current browser screenshot".into(),
            media_type: "image/png".into(),
        });
        model.data = json!({"image":{"source":name,"bytes":media.bytes,"width":media.width,"height":media.height}});
    }
    model.validate()?;
    Ok(model)
}
#[derive(Debug)]
struct Operate;
impl Operate {
    async fn execute(target: ActionTarget, input: ActionInput) -> rsi_ui::Result<SessionResult> {
        let action: Action = serde_json::from_value(input.value).map_err(error)?;
        let owner = target
            .context()
            .lookup_local::<SessionBrowserContract>()
            .ok_or(UiError::Retired)?;
        let field = |name: &str| input.fields.get(name).cloned().unwrap_or_default();
        let binding = || {
            action
                .binding
                .clone()
                .ok_or_else(|| error("browser identity missing"))
        };
        let operation = match action.operation.as_str() {
            "status" => SessionOperation::Status {},
            "open_public" => SessionOperation::Open {
                policy: SessionPolicy::PublicWeb {},
                url: field("url"),
            },
            "open_local" => {
                let url = field("url");
                let origin = url::Url::parse(&url)
                    .map_err(error)?
                    .origin()
                    .ascii_serialization();
                SessionOperation::Open {
                    policy: SessionPolicy::LocalDev { origin },
                    url,
                }
            }
            "navigate" => SessionOperation::Navigate {
                binding: binding()?,
                url: field("url"),
            },
            "observe" => SessionOperation::Observe {
                binding: binding()?,
            },
            "screenshot" => SessionOperation::Screenshot {
                binding: binding()?,
            },
            "close" => SessionOperation::Close {
                binding: binding()?,
            },
            "up" | "down" => SessionOperation::Scroll {
                binding: binding()?,
                direction: if action.operation == "up" {
                    "up"
                } else {
                    "down"
                }
                .into(),
            },
            "click" => SessionOperation::Click {
                binding: binding()?,
                document_version: action
                    .document_version
                    .ok_or_else(|| error("observe first"))?,
                observation_id: action
                    .observation_id
                    .ok_or_else(|| error("observe first"))?,
                node: field("node"),
            },
            "fill" => SessionOperation::Fill {
                binding: binding()?,
                document_version: action
                    .document_version
                    .ok_or_else(|| error("observe first"))?,
                observation_id: action
                    .observation_id
                    .ok_or_else(|| error("observe first"))?,
                node: field("node"),
                text: field("text"),
            },
            _ => return Err(error("unknown browser action")),
        };
        let result = owner
            .call(
                authority(target.context()).await?,
                operation,
                CancellationToken::new(),
            )
            .await
            .map_err(error)?;
        Ok(result)
    }
}
impl UiAction for Operate {
    fn invoke(
        &self,
        target: ActionTarget,
        input: ActionInput,
    ) -> BoxFuture<'static, rsi_ui::Result<UiView>> {
        Box::pin(async move { Ok(view(&Self::execute(target, input).await?)) })
    }
    fn invoke_model(
        &self,
        target: ActionTarget,
        input: ActionInput,
    ) -> BoxFuture<'static, rsi_ui::Result<UiModel>> {
        Box::pin(async move { model(&Self::execute(target, input).await?) })
    }
}
