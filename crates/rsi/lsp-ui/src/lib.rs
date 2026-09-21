//! Product Service UI adapter for the independent language provider.
#![forbid(unsafe_code)]
#![warn(missing_docs)]
use async_trait::async_trait;
use futures_util::future::BoxFuture;
use rsi_lsp as protocol;
use rsi_lsp::{LanguageContract, Location, Operation, Output, Query, QueryResult};
use rsi_meta::{
    ActivationPlan, ConfigValue, Context, MetaError, PluginFactory, PreparedActivation,
};
use rsi_ui::{
    ActionContribution, ActionInput, ActionTarget, Contributions, Result, SurfaceContribution,
    SurfaceRenderer, TargetKind, UiAction, UiContract, UiElement, UiError, UiView,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::VecDeque,
    path::PathBuf,
    sync::{Arc, Mutex, Weak},
};
use tokio_util::sync::CancellationToken;
/// Service-side read-only UI exported through the ordinary authenticated UI API.
#[derive(Debug, Clone, Default)]
pub struct LanguageUiFactory;
#[async_trait]
impl PluginFactory for LanguageUiFactory {
    fn prepare(&self, config: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        if !config.is_null() {
            return Err(MetaError::InvalidInput(
                "Language UI config must be null".into(),
            ));
        }
        Ok(PreparedActivation::new(config.clone())
            .requiring_local::<UiContract>()
            .requiring_local::<LanguageContract>())
    }
    async fn activate(&self, plan: ActivationPlan) -> rsi_meta::Result<()> {
        let lease = plan
            .local::<UiContract>()?
            .register(
                &plan,
                Contributions {
                    name: "rsi.lsp".into(),
                    surfaces: vec![SurfaceContribution {
                        name: "query".into(),
                        title: "Code intelligence".into(),
                        target: TargetKind::Surface,
                        renderer: Arc::new(Surface),
                    }],
                    actions: vec![ActionContribution {
                        name: "read".into(),
                        target: TargetKind::Surface,
                        handler: Arc::new(Read::default()),
                    }],
                    renderers: vec![],
                },
            )
            .map_err(|e| MetaError::Activation(e.to_string()))?;
        plan.defer(
            "withdraw language UI",
            Box::new(move || {
                Box::pin(async move {
                    if lease.dispose().await.is_clean() {
                        Ok(())
                    } else {
                        Err("Language UI cleanup failed".into())
                    }
                })
            }),
        )
    }
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum Action {
    Query { operation: Operation },
    Repeat { query: Query },
    Page { result: String, offset: usize },
    Open { location: Location },
    Start,
}
fn button(label: impl Into<String>, action: Action) -> UiElement {
    UiElement::Button {
        action: "read".into(),
        label: label.into(),
        value: serde_json::to_value(action).expect("closed language UI action"),
    }
}
fn text(value: impl Into<String>) -> UiElement {
    UiElement::Text { text: value.into() }
}
fn view(elements: Vec<UiElement>) -> UiView {
    UiView {
        title: "Code intelligence".into(),
        elements,
    }
}
fn start() -> UiView {
    let mut elements = vec![text(
        "Read current workspace code. Line and column are one-based Unicode characters. No edits are applied.",
    )];
    for (name, label, value) in [
        ("path", "Workspace file", "src/main.rs"),
        ("line", "Line", "1"),
        ("column", "Column", "1"),
    ] {
        elements.push(UiElement::Input {
            name: name.into(),
            label: label.into(),
            value: value.into(),
            multiline: false,
        });
    }
    for (label, operation) in [
        ("Find definition", Operation::Definition),
        ("Find references", Operation::References),
        ("Find implementation", Operation::Implementation),
        ("Read hover", Operation::Hover),
    ] {
        elements.push(button(label, Action::Query { operation }));
    }
    view(elements)
}
#[derive(Debug)]
struct Surface;
impl SurfaceRenderer for Surface {
    fn render(&self, _: &Context) -> Result<UiView> {
        Ok(start())
    }
}
fn error(e: impl std::fmt::Display) -> UiError {
    UiError::Action(e.to_string())
}
const MAXIMUM_RESULTS: usize = 64;
#[derive(Clone, Debug)]
struct Binding {
    session: String,
    workspace: PathBuf,
    provider: Weak<rsi_lsp::LanguageService>,
}
impl Binding {
    fn matches(&self, other: &Self) -> bool {
        self.session == other.session
            && self.workspace == other.workspace
            && self.provider.ptr_eq(&other.provider)
    }
}
#[derive(Debug, Default)]
struct Results {
    next: u64,
    entries: VecDeque<(String, Binding, Arc<Output>)>,
}
impl Results {
    fn insert(&mut self, binding: Binding, output: Arc<Output>) -> Result<String> {
        self.next = self.next.checked_add(1).ok_or(UiError::Capacity)?;
        let key = self.next.to_string();
        if self.entries.len() == MAXIMUM_RESULTS {
            self.entries.pop_front();
        }
        self.entries.push_back((key.clone(), binding, output));
        Ok(key)
    }
    fn get(&self, binding: &Binding, key: &str) -> Result<Arc<Output>> {
        self.entries.iter().find(|(id, owner, _)| id == key && owner.matches(binding))
            .map(|(_, _, output)| output.clone())
            .ok_or_else(|| error("Language result expired or belongs to another Session or provider; start a new query"))
    }
}
#[derive(Debug, Default)]
struct Read(Arc<Mutex<Results>>);
impl UiAction for Read {
    fn invoke(
        &self,
        target: ActionTarget,
        input: ActionInput,
    ) -> BoxFuture<'static, Result<UiView>> {
        let results = self.0.clone();
        Box::pin(async move {
            let stop = CancellationToken::new();
            let _guard = stop.clone().drop_guard();
            tokio::select! {biased;()=target.cancelled()=>Err(UiError::Retired),()=target.view_closed()=>Err(UiError::Retired),result=read(&results,target.context(),input,stop)=>visible_result(result)}
        })
    }
}
fn visible_result(result: Result<UiView>) -> Result<UiView> {
    match result {
        Err(UiError::Retired) => Err(UiError::Retired),
        Err(error) => {
            let message = error.to_string();
            Ok(view(vec![
                button("New language query", Action::Start),
                text(format!(
                    "Language read failed: {}",
                    &message[..message.floor_char_boundary(message.len().min(4096))]
                )),
            ]))
        }
        Ok(view) => Ok(view),
    }
}
async fn read(
    results: &Mutex<Results>,
    context: &Context,
    input: ActionInput,
    stop: CancellationToken,
) -> Result<UiView> {
    let action: Action = serde_json::from_value(input.value)
        .map_err(|_| UiError::Invalid("Invalid language action".into()))?;
    let controller = context
        .lookup_local::<rsi_client::SessionControllerContract>()
        .ok_or(UiError::Retired)?;
    let sessions = context
        .lookup_local::<rsi_session_protocol::SessionContract>()
        .ok_or(UiError::Retired)?;
    let service = context
        .lookup_local::<LanguageContract>()
        .ok_or(UiError::Retired)?;
    let header = sessions
        .attach(controller.session_id())
        .await
        .map_err(error)?
        .header()
        .await
        .map_err(error)?;
    let binding = Binding {
        session: controller.session_id().as_str().into(),
        workspace: header.canonical_cwd().into(),
        provider: Arc::downgrade(&service),
    };
    match action {
        Action::Start => {
            if !input.fields.is_empty() {
                return Err(error("Unexpected language fields"));
            }
            Ok(start())
        }
        Action::Query { operation } => {
            if input.fields.len() != 3 {
                return Err(error("Language query requires file, line and column"));
            }
            let field = |name: &str| {
                input
                    .fields
                    .get(name)
                    .ok_or_else(|| error("Missing language input"))
            };
            let query = Query {
                operation,
                path: field("path")?.clone(),
                line: field("line")?.parse().map_err(|_| error("Invalid line"))?,
                column: field("column")?
                    .parse()
                    .map_err(|_| error("Invalid column"))?,
            };
            let output = service
                .query(header.canonical_cwd().into(), query.clone(), stop)
                .await;
            query_view(results, binding, output, query)
        }
        Action::Repeat { query } => {
            if !input.fields.is_empty() {
                return Err(error("Unexpected language fields"));
            }
            let output = service
                .query(binding.workspace.clone(), query.clone(), stop)
                .await;
            query_view(results, binding, output, query)
        }
        Action::Page { result, offset } => {
            if !input.fields.is_empty() || offset > 128 {
                return Err(error("Invalid language result cursor"));
            }
            let output = results
                .lock()
                .expect("language results")
                .get(&binding, &result)?;
            if !matches!(&output.result, QueryResult::Locations{ locations } if offset < locations.len())
            {
                return Err(error("Invalid language result cursor"));
            }
            Ok(result_view(&output, &result, offset))
        }
        Action::Open { location } => {
            if !input.fields.is_empty() {
                return Err(error("Opening a location accepts no editable fields"));
            }
            protocol::relative(&location.path).map_err(error)?;
            location.range.validate().map_err(error)?;
            let source = service
                .current_file(header.canonical_cwd().into(), location.path.clone(), stop)
                .await
                .map_err(error)?;
            current_view(&location, &source)
        }
    }
}
fn query_view(
    results: &Mutex<Results>,
    binding: Binding,
    output: rsi_lsp::Result<Output>,
    query: Query,
) -> Result<UiView> {
    match output {
        Ok(output) => {
            let output = Arc::new(output);
            let key = results
                .lock()
                .expect("language results")
                .insert(binding, output.clone())?;
            Ok(result_view(&output, &key, 0))
        }
        Err(error) => Ok(view(vec![
            button("New language query", Action::Start),
            button("Repeat query", Action::Repeat { query }),
            text(format!("Language read failed: {error}")),
        ])),
    }
}
fn current_view(location: &Location, source: &str) -> Result<UiView> {
    let offset = protocol::byte_offset(source, location.range.start).map_err(error)?;
    let end = protocol::byte_offset(source, location.range.end).map_err(error)?;
    if end < offset {
        return Err(error("Invalid location range"));
    }
    let line_start = source[..offset].rfind('\n').map_or(0, |n| n + 1);
    let column = source[line_start..offset].chars().count() + 1;
    let window_start = source.floor_char_boundary(line_start.max(offset.saturating_sub(4096)));
    let end = source.floor_char_boundary((window_start + 12 * 1024).min(source.len()));
    Ok(view(vec![
        button("New language query", Action::Start),
        text(
            "Current file at the reported position. Later edits can make a language result stale.",
        ),
        UiElement::Field {
            label: "Location".into(),
            value: format!(
                "{}:{}:{}",
                location.path,
                location.range.start.line + 1,
                column
            ),
        },
        UiElement::Field {
            label: "Selected bytes in current file".into(),
            value: format!(
                "{offset}..{}",
                protocol::byte_offset(source, location.range.end).map_err(error)?
            ),
        },
        UiElement::Code {
            text: source[window_start..end].into(),
        },
    ]))
}
fn result_view(output: &Output, key: &str, offset: usize) -> UiView {
    let path = &output.query.path[..output
        .query
        .path
        .floor_char_boundary(output.query.path.len().min(512))];
    let mut elements = vec![
        button("New language query", Action::Start),
        button(
            "Repeat query",
            Action::Repeat {
                query: output.query.clone(),
            },
        ),
        UiElement::Field {
            label: "Query".into(),
            value: format!(
                "{:?} · {}:{}:{}",
                output.query.operation, path, output.query.line, output.query.column
            ),
        },
    ];
    match &output.result {
        QueryResult::Hover { text: content, .. } => elements.push(if content.is_empty() {
            text("No hover reported.")
        } else {
            UiElement::Code {
                text: content.clone(),
            }
        }),
        QueryResult::Locations { locations } => {
            if locations.is_empty() {
                elements.push(text("No locations reported. A newly started server may still be indexing; Repeat query reads current files again."));
            }
            let total = locations.len();
            let mut shown = 0;
            let mut bytes = 0;
            for location in locations.iter().skip(offset).take(16) {
                let action = button(
                    format!(
                        "Open {}:{} (UTF-16 column {})",
                        location.path,
                        location.range.start.line + 1,
                        location.range.start.character + 1
                    ),
                    Action::Open {
                        location: location.clone(),
                    },
                );
                let length = serde_json::to_vec(&action)
                    .expect("closed location button")
                    .len();
                if bytes + length > 80 * 1024 {
                    break;
                }
                bytes += length;
                shown += 1;
                elements.push(action);
            }
            if offset + shown < total {
                elements.push(text("More locations from this query are available. Repeat query reads current files again."));
                elements.push(button(
                    "More locations",
                    Action::Page {
                        result: key.into(),
                        offset: offset + shown,
                    },
                ));
            }
        }
    }
    view(elements)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cached_pages_preserve_results_and_reject_other_sessions_and_evicted_cursors() {
        let mut results = Results::default();
        let binding = Binding {
            session: "s".into(),
            workspace: "/workspace".into(),
            provider: Weak::new(),
        };
        let output = |line: u32| {
            Arc::new(Output {
                query: Query {
                    operation: Operation::References,
                    path: "src/main.rs".into(),
                    line: 1,
                    column: 1,
                },
                result: QueryResult::Locations {
                    locations: (0..32)
                        .map(|index| Location {
                            path: "src/main.rs".into(),
                            range: rsi_lsp::Range {
                                start: rsi_lsp::Position {
                                    line: line + index,
                                    character: 0,
                                },
                                end: rsi_lsp::Position {
                                    line: line + index,
                                    character: 0,
                                },
                            },
                        })
                        .collect(),
                },
            })
        };
        let key = results.insert(binding.clone(), output(0)).unwrap();
        results.insert(binding.clone(), output(100)).unwrap();
        let original = results.get(&binding, &key).unwrap();
        let second = result_view(&original, &key, 16);
        second.validate().unwrap();
        assert!(second.elements.iter().any(|e| matches!(e, UiElement::Button{label,..} if label == "Open src/main.rs:17 (UTF-16 column 1)")));
        assert!(
            !second
                .elements
                .iter()
                .any(|e| matches!(e, UiElement::Button{label,..} if label == "More locations"))
        );
        let mut other = binding.clone();
        other.session = "other".into();
        assert!(results.get(&other, &key).is_err());
        other = binding.clone();
        other.workspace = "/other".into();
        assert!(results.get(&other, &key).is_err());
        for _ in 0..MAXIMUM_RESULTS - 1 {
            results.insert(binding.clone(), output(200)).unwrap();
        }
        assert_eq!(results.entries.len(), MAXIMUM_RESULTS);
        assert!(results.get(&binding, &key).is_err());
        // Existing renderers keep their bounded Arc even when the cursor expires.
        assert!(
            matches!(&original.result, QueryResult::Locations{locations} if locations[16].range.start.line == 16)
        );
    }
    #[test]
    fn read_failure_is_visible_and_retirement_remains_terminal() {
        let view =
            visible_result(Err(UiError::Action("language protocol rejected".into()))).unwrap();
        view.validate().unwrap();
        assert!(view.elements.iter().any(
            |e| matches!(e,UiElement::Text{text} if text.contains("language protocol rejected"))
        ));
        assert!(matches!(
            visible_result(Err(UiError::Retired)),
            Err(UiError::Retired)
        ));
    }
    #[test]
    fn current_view_uses_scalar_column_and_rejects_mid_surrogate() {
        let source = "let emoji = \"😀\"; Bird\n";
        let start = protocol::position(source, 1, 18).unwrap();
        let location = Location {
            path: "src/main.rs".into(),
            range: rsi_lsp::Range {
                start,
                end: rsi_lsp::Position {
                    line: 0,
                    character: start.character + 4,
                },
            },
        };
        let view = current_view(&location, source).unwrap();
        view.validate().unwrap();
        assert!(view.elements.iter().any(|e|matches!(e,UiElement::Field{label,value}if label=="Location"&&value=="src/main.rs:1:18")));
        let invalid = Location {
            path: location.path,
            range: rsi_lsp::Range {
                start: rsi_lsp::Position {
                    line: 0,
                    character: 14,
                },
                end: rsi_lsp::Position {
                    line: 0,
                    character: 15,
                },
            },
        };
        assert!(current_view(&invalid, source).is_err());
    }
    #[test]
    fn result_pages_and_long_unicode_source_stay_inside_shared_view_bounds() {
        let query = Query {
            operation: Operation::References,
            path: "src/main.rs".into(),
            line: 1,
            column: 1,
        };
        let locations = (0..128)
            .map(|_| Location {
                path: "a".repeat(128),
                range: rsi_lsp::Range {
                    start: rsi_lsp::Position {
                        line: 0,
                        character: 0,
                    },
                    end: rsi_lsp::Position {
                        line: 0,
                        character: 0,
                    },
                },
            })
            .collect();
        let view = result_view(
            &Output {
                query,
                result: QueryResult::Locations { locations },
            },
            "1",
            0,
        );
        view.validate().unwrap();
        assert_eq!(
            view.elements
                .iter()
                .filter(|e| matches!(e,UiElement::Button{label,..}if label.starts_with("Open ")))
                .count(),
            16
        );
        assert!(
            view.elements
                .iter()
                .any(|e| matches!(e,UiElement::Button{label,..}if label=="More locations"))
        );
        assert!(
            view.elements.iter().any(|element| matches!(element,
                UiElement::Button {label, value, ..}
                if label == "More locations" && value.get("query").is_none()
            )),
            "pagination must address retained results, not rerun a query"
        );
        let source = "界😀".repeat(100_000);
        let start = protocol::position(&source, 1, 100_000).unwrap();
        let view = current_view(
            &Location {
                path: "a.rs".into(),
                range: rsi_lsp::Range { start, end: start },
            },
            &source,
        )
        .unwrap();
        view.validate().unwrap();
        assert!(
            view.elements
                .iter()
                .any(|e| matches!(e,UiElement::Code{text}if text.len()<=12*1024&&!text.is_empty()))
        );
    }
}
