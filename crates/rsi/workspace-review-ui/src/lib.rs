//! Shared, surface-bound workspace interval inspection.
#![forbid(unsafe_code)]
#![warn(missing_docs)]
use async_trait::async_trait;
use futures_util::future::BoxFuture;
use rsi_api_protocol::ApiClientContract;
use rsi_client::SessionControllerContract;
use rsi_meta::{
    ActivationPlan, ConfigValue, Context, MetaError, PluginFactory, PreparedActivation,
};
use rsi_session_protocol::SessionContract;
use rsi_ui::{
    ActionContribution, ActionInput, ActionTarget, Contributions, PresentationBinding,
    PresentationBindingOwner, PresentationIdentity, Result, SurfaceContribution, SurfaceRenderer,
    TargetKind, UiAction, UiContract, UiElement, UiError, UiModel, UiView,
};
use rsi_workspace_review_api::{Client, ConversationIdentity, Phase, Reply, Request, Scope};
use serde::{Deserialize, Serialize};
use std::sync::{Arc, Mutex, PoisonError};
use tokio_util::sync::CancellationToken;

type Selections = Mutex<Vec<(PresentationIdentity, Option<Operation>)>>;

/// Ordinary read-only contribution; no product-specific renderer transport.
#[derive(Debug, Clone, Default)]
pub struct Factory;
#[async_trait]
impl PluginFactory for Factory {
    fn prepare(&self, config: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        if !config.is_null() {
            return Err(MetaError::InvalidInput(
                "Workspace review UI configuration must be null".into(),
            ));
        }
        Ok(PreparedActivation::new(ConfigValue::Null).requiring_local::<UiContract>())
    }
    async fn activate(&self, plan: ActivationPlan) -> rsi_meta::Result<()> {
        let selections = Arc::new(Selections::default());
        let lease = plan
            .local::<UiContract>()?
            .register(
                &plan,
                Contributions {
                    name: "rsi.workspace.review".into(),
                    surfaces: vec![SurfaceContribution {
                        name: "review".into(),
                        title: "Workspace changes".into(),
                        target: TargetKind::Surface,
                        renderer: Arc::new(Surface(selections.clone())),
                    }],
                    actions: vec![ActionContribution {
                        name: "read".into(),
                        target: TargetKind::Surface,
                        handler: Arc::new(Read(selections)),
                    }],
                    renderers: vec![],
                },
            )
            .map_err(|e| MetaError::Activation(e.to_string()))?;
        plan.defer(
            "release workspace review UI",
            Box::new(move || {
                Box::pin(async move {
                    if lease.dispose().await.is_clean() {
                        Ok(())
                    } else {
                        Err("Workspace review UI cleanup failed".into())
                    }
                })
            }),
        )
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum Operation {
    List {
        after: Option<String>,
    },
    Files {
        id: String,
        offset: usize,
    },
    Diff {
        id: String,
        path: String,
        offset: usize,
    },
}
fn button(label: impl Into<String>, operation: Operation) -> UiElement {
    UiElement::Button {
        action: "read".into(),
        label: label.into(),
        value: serde_json::to_value(operation).expect("closed review action"),
    }
}
fn text(value: impl Into<String>) -> UiElement {
    UiElement::Text { text: value.into() }
}
fn home() -> UiElement {
    button("Review workspace changes", Operation::List { after: None })
}
fn view(elements: Vec<UiElement>) -> UiView {
    UiView {
        title: "Workspace changes".into(),
        elements,
    }
}
fn error(e: impl std::fmt::Display) -> UiError {
    let mut s = e.to_string();
    let mut end = s.len().min(4096);
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    s.truncate(end);
    UiError::Action(s)
}
fn cover(context: &Context) -> Result<UiView> {
    context
        .lookup_local::<SessionControllerContract>()
        .ok_or(UiError::Retired)?;
    Ok(view(vec![
        text(
            "Changes observed during an execution interval; concurrent edits may be included.",
        ),
        text(
            "Scope: Git tracked and non-ignored untracked text files. Ignored files and nested repository contents are outside coverage; omissions are reported per interval.",
        ),
        home(),
    ]))
}
fn record_selection(
    selections: &Selections,
    presentation: Option<&PresentationIdentity>,
    operation: Operation,
) {
    if let Some(presentation) = presentation
        && let Some((_, slot)) = selections
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter_mut()
            .find(|(id, _)| id == presentation)
    {
        *slot = Some(operation);
    }
}
fn selected_operation(
    selections: &Selections,
    presentation: &PresentationIdentity,
) -> Option<Operation> {
    selections
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .iter()
        .find_map(|(id, slot)| (id == presentation).then(|| slot.clone()))
        .flatten()
}
#[derive(Debug)]
struct BindingOwner {
    selections: Arc<Selections>,
    presentation: PresentationIdentity,
}
#[async_trait]
impl PresentationBindingOwner for BindingOwner {
    fn retire(&self) {
        self.selections
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .retain(|(id, _)| id != &self.presentation);
    }
    async fn close(&self) -> Result<()> {
        self.retire();
        Ok(())
    }
}
#[derive(Debug)]
struct Surface(Arc<Selections>);
impl SurfaceRenderer for Surface {
    fn bind(
        &self,
        target: Context,
        presentation: PresentationIdentity,
        stop: CancellationToken,
    ) -> BoxFuture<'_, Result<Option<PresentationBinding>>> {
        Box::pin(async move {
            if stop.is_cancelled() {
                return Err(UiError::Retired);
            }
            {
                let mut entries = self.0.lock().unwrap_or_else(PoisonError::into_inner);
                if let Some((_, slot)) = entries.iter_mut().find(|(id, _)| id == &presentation) {
                    *slot = None;
                } else {
                    entries.push((presentation.clone(), None));
                }
            }
            let owner = Arc::new(BindingOwner {
                selections: self.0.clone(),
                presentation,
            });
            if stop.is_cancelled() {
                owner.retire();
                return Err(UiError::Retired);
            }
            Ok(Some(PresentationBinding::new(target, owner)))
        })
    }
    fn render(&self, context: &Context) -> Result<UiView> {
        cover(context)
    }
    fn model_in(
        &self,
        target: Context,
        presentation: PresentationIdentity,
    ) -> BoxFuture<'_, Result<UiModel>> {
        let selected = selected_operation(&self.0, &presentation);
        Box::pin(async move {
            let current = match selected {
                Some(operation) => read(&target, operation).await?,
                None => cover(&target)?,
            };
            UiModel::standard(current).map_err(Into::into)
        })
    }
}
#[derive(Debug)]
struct Read(Arc<Selections>);
impl UiAction for Read {
    fn invoke(
        &self,
        target: ActionTarget,
        input: ActionInput,
    ) -> BoxFuture<'static, Result<UiView>> {
        let selections = self.0.clone();
        Box::pin(async move {
            if !input.fields.is_empty() {
                return Err(UiError::Invalid(
                    "Workspace review has no editable fields".into(),
                ));
            }
            let operation: Operation = serde_json::from_value(input.value).map_err(error)?;
            let current = tokio::select! {biased;
                ()=target.cancelled()=>Err(UiError::Retired),
                ()=target.view_closed()=>Err(UiError::Retired),
                result=read(target.context(),operation.clone())=>result,
            }?;
            record_selection(&selections, target.presentation(), operation);
            Ok(current)
        })
    }
}
async fn read(context: &Context, operation: Operation) -> Result<UiView> {
    let controller = context
        .lookup_local::<SessionControllerContract>()
        .ok_or(UiError::Retired)?;
    let session = context
        .lookup_local::<SessionContract>()
        .ok_or(UiError::Retired)?;
    let api = context
        .lookup_local::<ApiClientContract>()
        .ok_or_else(|| error("Workspace review API unavailable"))?;
    let handle = session
        .attach(controller.session_id())
        .await
        .map_err(error)?;
    let header = handle.header().await.map_err(error)?;
    let scope = Scope {
        workspace: rsi_workspace_protocol::WorkspaceKey::from_coordinates(header.coordinates()),
        conversation: ConversationIdentity::Native(controller.session_id().clone()),
    };
    let request = match operation {
        Operation::List { after } => Request::List { scope, after },
        Operation::Files { id, offset } => Request::Files { scope, id, offset },
        Operation::Diff { id, path, offset } => Request::Diff {
            scope,
            id,
            path,
            offset,
        },
    };
    Ok(present(
        Client::new(api).call(request).await.map_err(error)?,
    ))
}
fn present(reply: Reply) -> UiView {
    let mut elements = vec![home()];
    match reply {
        Reply::Summaries { items, next } => present_summaries(&mut elements, items, next),
        Reply::Files {
            id,
            offset,
            files,
            has_more,
        } => present_files(&mut elements, id, offset, &files, has_more),
        Reply::Diff {
            id,
            path,
            offset,
            has_more,
            text: content,
            ..
        } => present_diff(&mut elements, id, path, offset, has_more, &content),
        Reply::Expired { .. } => elements.push(text("This comparison has expired.")),
    }
    view(elements)
}
fn present_summaries(
    elements: &mut Vec<UiElement>,
    items: Vec<rsi_workspace_review_api::Summary>,
    next: Option<String>,
) {
    if items.is_empty() {
        elements.push(text("No workspace changes to review."));
    } else {
        elements.push(text("Interval evidence includes concurrent edits, and excludes ignored files and nested repository contents. Complete applies only to the declared text-file scope and controlled work. External agents and detached background work may write after the interval."));
    }
    for item in items {
        let state = match item.phase {
            Phase::Pending => "Pending; work or capture not settled",
            Phase::Complete => "Complete",
            Phase::Partial => "Partial",
        };
        let value = match item.finished_ms {
            Some(end) => format!(
                "{state} · {} {} · +{} / −{} lines · {} → {end} ms",
                item.changed_files,
                if item.changed_files == 1 {
                    "file"
                } else {
                    "files"
                },
                item.added_lines,
                item.removed_lines,
                item.started_ms,
            ),
            None => state.to_owned(),
        };
        elements.push(UiElement::Field {
            label: format!("Interval {}", item.id),
            value,
        });
        if !item.omissions.is_empty() {
            elements.push(text(format!(
                "Coverage omissions: {}",
                item.omissions
                    .iter()
                    .map(|o| format!("{:?}: {}", o.kind, o.count))
                    .collect::<Vec<_>>()
                    .join(", ")
            )));
        }
        if item.changed_files > 0 && item.phase != Phase::Pending {
            elements.push(button(
                "Open interval files",
                Operation::Files {
                    id: item.id,
                    offset: 0,
                },
            ));
        }
    }
    if let Some(after) = next {
        elements.push(button(
            "More intervals",
            Operation::List { after: Some(after) },
        ));
    }
}
fn present_files(
    elements: &mut Vec<UiElement>,
    id: String,
    offset: usize,
    files: &[rsi_workspace_review_api::FileChange],
    has_more: bool,
) {
    elements.push(text(format!(
        "Interval {id}. Changes occurred in this interval; concurrent edits may be included."
    )));
    let shown = files
        .iter()
        .take(16)
        .scan(0usize, |bytes, file| {
            *bytes +=
                2 * file.path.len() + file.previous_path.as_ref().map_or(0, String::len) + 512;
            Some(*bytes)
        })
        .take_while(|bytes| *bytes <= 16 * 1024)
        .count();
    if files.is_empty() {
        elements.push(text("No comparable text changes in this interval."));
    }
    for file in files.iter().take(shown) {
        elements.push(UiElement::Field {
            label: file.path.clone(),
            value: format!(
                "+{} / −{}{}",
                file.added,
                file.removed,
                file.previous_path
                    .as_ref()
                    .map(|p| format!(" · renamed from {p}"))
                    .unwrap_or_default()
            ),
        });
        elements.push(button(
            "Open file diff",
            Operation::Diff {
                id: id.clone(),
                path: file.path.clone(),
                offset: 0,
            },
        ));
    }
    if has_more || shown < files.len() {
        elements.push(button(
            "More changed files",
            Operation::Files {
                id,
                offset: offset + shown,
            },
        ));
    }
}
fn present_diff(
    elements: &mut Vec<UiElement>,
    id: String,
    path: String,
    offset: usize,
    has_more: bool,
    content: &str,
) {
    let mut end = content.len().min(12 * 1024);
    while !content.is_char_boundary(end) {
        end -= 1;
    }
    elements.push(button(
        "Back to interval files",
        Operation::Files {
            id: id.clone(),
            offset: 0,
        },
    ));
    elements.push(UiElement::Field {
        label: "File diff".into(),
        value: path.clone(),
    });
    elements.push(UiElement::Code {
        text: content[..end].into(),
    });
    if has_more || end < content.len() {
        elements.push(button(
            "Continue diff",
            Operation::Diff {
                id,
                path,
                offset: offset + end,
            },
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rsi_workspace_review_api::FileChange;
    #[test]
    fn empty_panel_reports_no_changes() {
        let result = present(Reply::Summaries {
            items: vec![],
            next: None,
        });
        assert!(result.elements.iter().any(|element| matches!(element, UiElement::Text { text } if text == "No workspace changes to review.")));
    }
    #[test]
    fn review_rows_show_only_observed_counts_and_available_actions() {
        for (phase, files, expected, opens) in [
            (
                Phase::Pending,
                0,
                "Pending; work or capture not settled",
                false,
            ),
            (Phase::Complete, 1, "Complete · 1 file", true),
            (Phase::Partial, 2, "Partial · 2 files", true),
        ] {
            let item = serde_json::from_value(serde_json::json!({
                "id": "1".repeat(32), "scope": {
                    "workspace": "2".repeat(64), "conversation": {"kind":"native","id":"source"}
                },
                "turn":"turn", "execution":"1", "accepted_seq":"1", "live_seq":"3",
                "started_ms":"100", "finished_ms": (phase != Phase::Pending).then_some("200"),
                "phase":phase, "changed_files":files, "added_lines":0, "removed_lines":0, "omissions":[]
            })).unwrap();
            let result = present(Reply::Summaries {
                items: vec![item],
                next: None,
            });
            let value = result
                .elements
                .iter()
                .find_map(|element| match element {
                    UiElement::Field { value, .. } => Some(value.as_str()),
                    _ => None,
                })
                .unwrap();
            if phase == Phase::Pending {
                assert_eq!(value, expected);
            } else {
                assert_eq!(value, format!("{expected} · +0 / −0 lines · 100 → 200 ms"));
            }
            assert_eq!(opens, result.elements.iter().any(|element| matches!(element, UiElement::Button { label, .. } if label == "Open interval files")));
        }
    }
    fn next(view: &UiView) -> Operation {
        let value = view
            .elements
            .iter()
            .rev()
            .find_map(|element| match element {
                UiElement::Button { value, .. } => Some(value.clone()),
                _ => None,
            })
            .unwrap();
        serde_json::from_value(value).unwrap()
    }
    #[test]
    fn diff_pages_advance_only_over_displayed_utf8_and_remain_bounded() {
        let content = "🦀\u{1}".repeat(12_000);
        let result = present(Reply::Diff {
            id: "a".repeat(32),
            path: "file.rs".into(),
            offset: 7,
            next_offset: 7 + content.len(),
            has_more: false,
            text: content.clone(),
        });
        result.validate().unwrap();
        let shown = result
            .elements
            .iter()
            .find_map(|e| match e {
                UiElement::Code { text } => Some(text),
                _ => None,
            })
            .unwrap();
        assert!(content.starts_with(shown));
        assert!(shown.len() <= 12 * 1024);
        assert!(matches!(next(&result),Operation::Diff{offset,..} if offset==7+shown.len()));
    }
    #[test]
    fn long_untrusted_names_are_paged_without_losing_entries() {
        let files = (0..64)
            .map(|i| FileChange {
                path: format!("{i}{}", "\u{1}".repeat(4000)),
                previous_path: Some("x".repeat(4096)),
                added: 1,
                removed: 2,
            })
            .collect::<Vec<_>>();
        let result = present(Reply::Files {
            id: "b".repeat(32),
            offset: 32,
            files,
            has_more: false,
        });
        result.validate().unwrap();
        assert!(matches!(next(&result), Operation::Files { offset: 33, .. }));
    }
    #[test]
    fn action_payload_cannot_substitute_a_source() {
        assert!(
            serde_json::from_value::<Operation>(
                serde_json::json!({"kind":"list","after":null,"scope":{"workspace":"attacker"}})
            )
            .is_err()
        );
        let result = present(Reply::Expired { id: "a".repeat(32) });
        result.validate().unwrap();
        assert!(
            result.elements.iter().any(
                |e| matches!(e,UiElement::Text{text} if text == "This comparison has expired.")
            )
        );
    }
    #[tokio::test]
    async fn presentation_selection_tracks_bound_lifetime_and_rejects_retired_updates() {
        let runtime = rsi_meta::Runtime::default();
        let selections = Arc::new(Selections::default());
        let surface = Surface(selections.clone());
        let id = PresentationIdentity {
            reference: rsi_ui::UiReference {
                application: "app".into(),
                target: "target".into(),
                contribution: "rsi.workspace.review".into(),
                name: "review".into(),
            },
            epoch: "epoch".into(),
        };
        let cancelled = CancellationToken::new();
        cancelled.cancel();
        assert!(matches!(
            surface.bind(runtime.root(), id.clone(), cancelled).await,
            Err(UiError::Retired)
        ));
        assert!(selections.lock().unwrap().is_empty());
        let binding = surface
            .bind(runtime.root(), id.clone(), CancellationToken::new())
            .await
            .unwrap()
            .expect("presentation binding");
        assert_eq!(selected_operation(&selections, &id), None);
        assert_eq!(selections.lock().unwrap().len(), 1);
        let selected = Operation::Files {
            id: "1".repeat(32),
            offset: 0,
        };
        record_selection(&selections, Some(&id), selected.clone());
        assert_eq!(selected_operation(&selections, &id), Some(selected));
        binding.close().await.unwrap();
        assert!(selections.lock().unwrap().is_empty());
        record_selection(&selections, Some(&id), Operation::List { after: None });
        assert!(selections.lock().unwrap().is_empty());
        assert!(runtime.shutdown().await.is_clean());
    }
}
