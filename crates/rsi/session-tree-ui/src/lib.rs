//! Ordinary finite Agent-tree and child-history UI contributions.
#![forbid(unsafe_code)]
#![warn(missing_docs)]
#![allow(clippy::missing_errors_doc)]

mod history;
mod tree;

use async_trait::async_trait;
use futures_util::future::BoxFuture;
use rsi_agent_session_protocol::SessionId;
use rsi_client::{SessionController, SessionControllerContract};
use rsi_meta::{
    ActivationPlan, ConfigValue, Context, LocalContract, MetaError, PluginFactory,
    PreparedActivation,
};
use rsi_session_protocol::{SessionContract, SessionService};
use rsi_ui::{
    ActionContribution, ActionInput, ActionTarget, Contributions, Result, SurfaceContribution,
    SurfaceRenderer, TargetKind, UiAction, UiContract, UiElement, UiError, UiView,
};
use serde::{Deserialize, Serialize};
use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};
use tokio_util::sync::CancellationToken;

const TREE_PAGE_ROWS: usize = 32;
const HISTORY_PAGE_FACTS: usize = 64;
const RECORD_PREVIEW_BYTES: usize = 512;
const SOURCE_PAGE_ROWS: usize = 32;
const SOURCE_PAGE_BYTES: usize = 16 * 1024;
static NEXT_READER: AtomicU64 = AtomicU64::new(1);

/// Exact surface-bound finite reader; no observation or execution is started.
#[derive(Debug)]
pub struct TreeReader {
    revision: String,
    controller: Arc<SessionController>,
    service: Arc<dyn SessionService>,
    stop: CancellationToken,
}
/// Local mapping shared by the actual surface's target and contribution callbacks.
#[derive(Debug)]
pub struct TreeReaderContract;
impl LocalContract for TreeReaderContract {
    const KEY: &'static str = "rsi.session.tree.reader";
    type Service = TreeReader;
}

/// Captures the actual surface controller and Session provider under Meta dependencies.
#[derive(Debug, Clone, Default)]
pub struct TreeTargetFactory;
#[async_trait]
impl PluginFactory for TreeTargetFactory {
    fn prepare(&self, desired: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        Ok(no_config(desired)?
            .requiring_local::<SessionControllerContract>()
            .requiring_local::<SessionContract>())
    }
    async fn activate(&self, plan: ActivationPlan) -> rsi_meta::Result<()> {
        let revision = NEXT_READER
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |value| {
                value.checked_add(1)
            })
            .map_err(|_| MetaError::Activation("Agent tree reader revision exhausted".into()))?
            .to_string();
        let reader = Arc::new(TreeReader {
            revision,
            controller: plan.local::<SessionControllerContract>()?,
            service: plan.local::<SessionContract>()?,
            stop: CancellationToken::new(),
        });
        let supply = plan
            .context()
            .provide_local::<TreeReaderContract>(reader.clone())?;
        plan.defer(
            "withdraw finite Agent tree reader",
            Box::new(move || {
                Box::pin(async move {
                    reader.stop.cancel();
                    drop(supply);
                    Ok(())
                })
            }),
        )
    }
}

/// First-party read-only Agent tree, breadcrumbs and paged child inspector.
#[derive(Debug, Clone, Default)]
pub struct SessionTreeUiFactory;
#[async_trait]
impl PluginFactory for SessionTreeUiFactory {
    fn prepare(&self, desired: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        Ok(no_config(desired)?.requiring_local::<UiContract>())
    }
    async fn activate(&self, plan: ActivationPlan) -> rsi_meta::Result<()> {
        let lease = plan
            .local::<UiContract>()?
            .register(
                &plan,
                Contributions {
                    name: "rsi.session.tree".into(),
                    surfaces: vec![SurfaceContribution {
                        name: "tree".into(),
                        title: "Agent tree".into(),
                        target: TargetKind::Surface,
                        renderer: Arc::new(TreeSurface),
                    }],
                    actions: vec![ActionContribution {
                        name: "read".into(),
                        target: TargetKind::Surface,
                        handler: Arc::new(Read),
                    }],
                    renderers: vec![],
                },
            )
            .map_err(meta)?;
        plan.defer(
            "release Agent tree UI contribution",
            Box::new(move || {
                Box::pin(async move {
                    if lease.dispose().await.is_clean() {
                        Ok(())
                    } else {
                        Err("Agent tree UI cleanup failed".into())
                    }
                })
            }),
        )
    }
}
fn no_config(value: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
    if !value.is_null() {
        return Err(MetaError::InvalidInput(
            "Agent tree UI configuration must be null".into(),
        ));
    }
    Ok(PreparedActivation::new(ConfigValue::Null))
}
fn meta(error: impl std::fmt::Display) -> MetaError {
    MetaError::Activation(error.to_string())
}
fn action_error(error: impl std::fmt::Display) -> UiError {
    UiError::Action(
        rsi_conversation::FieldWindow::text(&error.to_string(), 0, 4096)
            .expect("valid diagnostic bound")
            .text,
    )
}
fn reader(context: &Context) -> Result<Arc<TreeReader>> {
    context
        .lookup_local::<TreeReaderContract>()
        .filter(|reader| !reader.stop.is_cancelled())
        .ok_or(UiError::Retired)
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Request {
    revision: String,
    operation: Operation,
}
#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum Operation {
    Tree {
        selected: Option<SessionId>,
        after: Option<SessionId>,
    },
    History {
        selected: SessionId,
        before: Option<String>,
        watermark: Option<String>,
    },
    Sources {
        selected: SessionId,
        seq: String,
        start: u16,
    },
    Source {
        selected: SessionId,
        source: rsi_conversation::SourceRef,
        start: String,
    },
}
fn button(reader: &TreeReader, label: impl Into<String>, operation: Operation) -> UiElement {
    UiElement::Button {
        action: "read".into(),
        label: label.into(),
        value: serde_json::to_value(Request {
            revision: reader.revision.clone(),
            operation,
        })
        .expect("closed Agent tree action"),
    }
}
fn field(label: &str, value: impl Into<String>) -> UiElement {
    UiElement::Field {
        label: label.into(),
        value: value.into(),
    }
}
fn decimal(value: &str) -> Result<u64> {
    if value.is_empty()
        || value.len() > 20
        || value.starts_with('0') && value != "0"
        || !value.bytes().all(|byte| byte.is_ascii_digit())
    {
        return Err(UiError::Invalid("Invalid decimal cursor".into()));
    }
    value
        .parse()
        .map_err(|_| UiError::Invalid("Cursor exceeds u64".into()))
}
#[derive(Debug)]
struct TreeSurface;
impl SurfaceRenderer for TreeSurface {
    fn render(&self, context: &Context) -> Result<UiView> {
        let reader = reader(context)?;
        Ok(UiView {
            title: "Agent tree".into(),
            elements: vec![
                field("Root Session", reader.controller.session_id().to_string()),
                UiElement::Text {
                    text: "Read-only snapshots of agents and their conversation history.".into(),
                },
                button(
                    &reader,
                    "Inspect agent tree",
                    Operation::Tree {
                        selected: None,
                        after: None,
                    },
                ),
            ],
        })
    }
}
#[derive(Debug)]
struct Read;
impl UiAction for Read {
    fn invoke(
        &self,
        target: ActionTarget,
        input: ActionInput,
    ) -> BoxFuture<'static, Result<UiView>> {
        Box::pin(async move {
            if !input.fields.is_empty() {
                return Err(UiError::Invalid(
                    "Agent inspection accepts no editable fields".into(),
                ));
            }
            let request: Request = serde_json::from_value(input.value)
                .map_err(|error| UiError::Invalid(error.to_string()))?;
            let reader = reader(target.context())?;
            if request.revision != reader.revision {
                return Err(UiError::Retired);
            }
            let read = tree::read(
                &reader,
                target.context().runtime().execution(),
                request.operation,
            );
            tokio::select! { biased;
                () = target.cancelled() => Err(UiError::Retired),
                () = target.view_closed() => Err(UiError::Retired),
                () = reader.stop.cancelled() => Err(UiError::Retired),
                result = read => result,
            }
        })
    }
}
