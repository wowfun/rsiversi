//! Surface-bound standard presentation of authenticated service contributions.
#![forbid(unsafe_code)]
#![warn(missing_docs)]
use async_trait::async_trait;
use futures_util::{FutureExt as _, future::BoxFuture};
use rsi_meta::{
    ActivationPlan, ConfigValue, Context, LocalContract, MetaError, PluginFactory,
    PreparedActivation,
};
use rsi_ui::{
    ActionContribution, ActionInput, ActionTarget, Contributions, Result, SurfaceContribution,
    SurfaceRenderer, TargetKind, UiAction, UiContract, UiElement, UiError, UiView,
};
use rsi_ui_api::{
    CatalogCursor, CatalogPage, CatalogRequest, ExportScope, Invoke, Observe, Selection, UiClient,
    UiItem, UiObservation,
};
use serde::{Deserialize, Serialize};
use std::{sync::Arc, time::Duration};
use tokio_util::sync::CancellationToken;
struct Remote {
    application: String,
    observation: UiObservation,
    item: UiItem,
}
#[derive(Default)]
struct State {
    catalog: Option<CatalogPage>,
    remote: Option<Remote>,
}
/// Captured reader for one actual Session target and API connection.
pub struct Reader {
    revision: String,
    scope: ExportScope,
    client: UiClient,
    state: tokio::sync::Mutex<State>,
    stop: CancellationToken,
}
impl std::fmt::Debug for Reader {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ServiceUiReader")
            .field("revision", &self.revision)
            .finish_non_exhaustive()
    }
}
/// Surface-local service UI reader.
#[derive(Debug)]
pub struct ReaderContract;
impl LocalContract for ReaderContract {
    const KEY: &'static str = "rsi.service.ui.reader";
    type Service = Reader;
}
/// Captures only dependencies of the actual local Session surface.
#[derive(Debug, Clone, Default)]
pub struct TargetFactory;
#[async_trait]
impl PluginFactory for TargetFactory {
    fn prepare(&self, config: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        Ok(no_config(config)?
            .requiring_local::<rsi_client::SessionControllerContract>()
            .requiring_local::<rsi_api_protocol::ApiClientContract>())
    }
    async fn activate(&self, plan: ActivationPlan) -> rsi_meta::Result<()> {
        let reader = Arc::new(Reader {
            revision: rsi_ui::fresh_identity("service-ui").map_err(meta)?,
            scope: ExportScope {
                kind: "session".into(),
                key: plan
                    .local::<rsi_client::SessionControllerContract>()?
                    .session_id()
                    .to_string(),
            },
            client: UiClient::new(plan.local::<rsi_api_protocol::ApiClientContract>()?)
                .map_err(meta)?,
            state: tokio::sync::Mutex::new(State::default()),
            stop: CancellationToken::new(),
        });
        let supply = plan
            .context()
            .provide_local::<ReaderContract>(reader.clone())?;
        plan.defer(
            "release service UI observation",
            Box::new(move || {
                Box::pin(async move {
                    reader.stop.cancel();
                    drop(supply);
                    reader.state.lock().await.remote.take();
                    Ok(())
                })
            }),
        )
    }
}
/// Local contribution for service catalogs and their standard views.
#[derive(Debug, Clone, Default)]
pub struct Factory;
#[async_trait]
impl PluginFactory for Factory {
    fn prepare(&self, config: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        Ok(no_config(config)?.requiring_local::<UiContract>())
    }
    async fn activate(&self, plan: ActivationPlan) -> rsi_meta::Result<()> {
        let lease = plan
            .local::<UiContract>()?
            .register(
                &plan,
                Contributions {
                    name: "rsi.service.ui.client".into(),
                    surfaces: vec![SurfaceContribution {
                        name: "catalog".into(),
                        title: "Service extensions".into(),
                        target: TargetKind::Surface,
                        renderer: Arc::new(Surface),
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
            "release service UI client",
            Box::new(move || {
                Box::pin(async move {
                    if lease.dispose().await.is_clean() {
                        Ok(())
                    } else {
                        Err("Service UI cleanup failed".into())
                    }
                })
            }),
        )
    }
}
fn no_config(config: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
    if !config.is_null() {
        return Err(MetaError::InvalidInput(
            "Service UI config must be null".into(),
        ));
    }
    Ok(PreparedActivation::new(config.clone()))
}
fn meta(e: impl std::fmt::Display) -> MetaError {
    MetaError::Activation(e.to_string())
}
fn error(e: impl std::fmt::Display) -> UiError {
    let text = e.to_string();
    UiError::Action(text[..text.floor_char_boundary(text.len().min(4096))].into())
}
fn reader(context: &Context) -> Result<Arc<Reader>> {
    context
        .lookup_local::<ReaderContract>()
        .filter(|reader| !reader.stop.is_cancelled())
        .ok_or(UiError::Retired)
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    revision: String,
    operation: Operation,
}
#[derive(Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum Operation {
    List {
        after: Option<CatalogCursor>,
    },
    Open {
        bundle: String,
        surface: String,
    },
    Invoke {
        application: String,
        snapshot: String,
        action: String,
        value: serde_json::Value,
    },
    Close,
}
fn button(reader: &Reader, label: impl Into<String>, operation: Operation) -> UiElement {
    UiElement::Button {
        action: "read".into(),
        label: label.into(),
        value: serde_json::to_value(Request {
            revision: reader.revision.clone(),
            operation,
        })
        .expect("closed service UI action"),
    }
}
fn initial(reader: &Reader) -> UiView {
    UiView {
        title: "Service extensions".into(),
        elements: vec![
            UiElement::Text {
                text: "Inspect standard service views for this Session.".into(),
            },
            button(
                reader,
                "List service extensions",
                Operation::List { after: None },
            ),
        ],
    }
}
#[derive(Debug)]
struct Surface;
impl SurfaceRenderer for Surface {
    fn render(&self, context: &Context) -> Result<UiView> {
        Ok(initial(reader(context)?.as_ref()))
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
            let reader = reader(target.context())?;
            let request: Request = serde_json::from_value(input.value)
                .map_err(|_| error("Invalid service UI action"))?;
            if request.revision != reader.revision {
                return Err(UiError::Retired);
            }
            let mut state = reader.state.try_lock().map_err(|_| UiError::Capacity)?;
            let future = read(&reader, &mut state, request.operation, input.fields);
            tokio::select! {biased;()=target.cancelled()=>Err(UiError::Retired),()=target.view_closed()=>Err(UiError::Retired),()=reader.stop.cancelled()=>Err(UiError::Retired),result=tokio::time::timeout(Duration::from_secs(40),future)=>result.map_err(|_|error("Service UI read deadline"))?}
        })
    }
}
async fn read(
    reader: &Reader,
    state: &mut State,
    operation: Operation,
    fields: std::collections::BTreeMap<String, String>,
) -> Result<UiView> {
    if !matches!(operation, Operation::Invoke { .. } | Operation::Close) && !fields.is_empty() {
        return Err(error("Unexpected service UI fields"));
    }
    match operation {
        Operation::Close => {
            state.remote = None;
            state.catalog = None;
            Ok(initial(reader))
        }
        Operation::List { after } => {
            state.remote = None;
            let page = reader
                .client
                .catalog(&CatalogRequest {
                    scope: reader.scope.clone(),
                    after,
                    maximum: 64,
                })
                .await
                .map_err(error)?;
            let view = catalog_view(reader, &page);
            state.catalog = Some(page);
            Ok(view)
        }
        Operation::Open { bundle, surface } => {
            if !state.catalog.as_ref().is_some_and(|page| {
                page.entries
                    .iter()
                    .any(|e| e.bundle == bundle && e.surface == surface)
            }) {
                return Err(error("Service view is outside the displayed catalog"));
            }
            state.remote = None;
            let application = rsi_ui::fresh_identity("terminal-service-ui").map_err(error)?;
            let mut observation = reader
                .client
                .observe(&Observe {
                    application: application.clone(),
                    selections: vec![Selection {
                        scope: reader.scope.clone(),
                        bundle,
                        surface,
                    }],
                })
                .await
                .map_err(error)?;
            let item = observation
                .next()
                .await
                .map_err(error)?
                .ok_or(UiError::Retired)?;
            let result = present(reader, &application, &item)?;
            state.remote = Some(Remote {
                application,
                observation,
                item,
            });
            Ok(result)
        }
        Operation::Invoke {
            application,
            snapshot,
            action,
            value,
        } => {
            invoke(
                reader,
                state,
                application,
                snapshot,
                action,
                ActionInput { value, fields },
            )
            .await
        }
    }
}
fn catalog_view(reader: &Reader, page: &CatalogPage) -> UiView {
    let mut elements = vec![];
    for entry in &page.entries {
        elements.push(button(
            reader,
            &entry.title,
            Operation::Open {
                bundle: entry.bundle.clone(),
                surface: entry.surface.clone(),
            },
        ));
    }
    if let Some(after) = &page.next {
        elements.push(button(
            reader,
            "More service extensions",
            Operation::List {
                after: Some(after.clone()),
            },
        ));
    }
    if elements.is_empty() {
        elements.push(UiElement::Text {
            text: "No standard service views available.".into(),
        });
    }
    UiView {
        title: "Service extensions".into(),
        elements,
    }
}
async fn invoke(
    reader: &Reader,
    state: &mut State,
    application: String,
    snapshot: String,
    action: String,
    input: ActionInput,
) -> Result<UiView> {
    let Some(mut remote) = state.remote.take() else {
        return Err(UiError::Retired);
    };
    if application != remote.application
        || snapshot != remote.item.item.snapshot.revision.to_string()
    {
        state.remote = Some(remote);
        return Err(UiError::Retired);
    }
    for _ in 0..16 {
        let Some(next) = remote.observation.next().now_or_never() else {
            break;
        };
        remote.item = next.map_err(error)?.ok_or(UiError::Retired)?;
    }
    if snapshot != remote.item.item.snapshot.revision.to_string() {
        let mut view = present(reader, &remote.application, &remote.item)?;
        view.elements.insert(
            0,
            UiElement::Text {
                text: "Service view changed. Review the values and choose the action again.".into(),
            },
        );
        view.validate().map_err(error)?;
        state.remote = Some(remote);
        return Ok(view);
    }
    if !remote
        .item
        .item
        .snapshot
        .model
        .actions
        .iter()
        .any(|a| a.name == action)
    {
        state.remote = Some(remote);
        return Err(error("Action is outside displayed service view"));
    }
    let previous = remote.item.item.snapshot.revision;
    let request = Invoke {
        application: remote.application.clone(),
        action: rsi_ui::PresentationAction {
            presentation: remote.item.item.snapshot.presentation.clone(),
            revision: previous,
            action,
        },
        ticket: remote.item.item.ticket.take().ok_or(UiError::Retired)?,
        input,
    };
    reader.client.invoke(&request).await.map_err(error)?;
    for _ in 0..16 {
        remote.item = remote
            .observation
            .next()
            .await
            .map_err(error)?
            .ok_or(UiError::Retired)?;
        if remote.item.item.snapshot.revision > previous && remote.item.item.ticket.is_some() {
            let result = present(reader, &remote.application, &remote.item)?;
            state.remote = Some(remote);
            return Ok(result);
        }
    }
    Err(error("Service view did not produce a new bounded snapshot"))
}
fn present(reader: &Reader, application: &str, item: &UiItem) -> Result<UiView> {
    let mut view = item
        .item
        .snapshot
        .model
        .standard_view
        .clone()
        .ok_or_else(|| error("This extension has no standard terminal presentation"))?;
    for element in &mut view.elements {
        if let UiElement::Button { action, value, .. } = element {
            *value = serde_json::to_value(Request {
                revision: reader.revision.clone(),
                operation: Operation::Invoke {
                    application: application.into(),
                    snapshot: item.item.snapshot.revision.to_string(),
                    action: action.clone(),
                    value: value.clone(),
                },
            })
            .expect("bounded service action");
            *action = "read".into();
        }
    }
    // Closing is local control and must not forward the remote form's editable fields.
    view.elements
        .push(button(reader, "Close service view", Operation::Close));
    view.validate().map_err(error)?;
    Ok(view)
}

#[cfg(test)]
mod tests;
