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
    ActionContribution, ActionInput, ActionTarget, Contributions, Result, SurfaceContribution,
    SurfaceRenderer, TargetKind, UiAction, UiContract, UiElement, UiError, UiView,
};
use rsi_workspace_review_api::{Client, ConversationIdentity, Phase, Reply, Request, Scope};
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use std::sync::Arc;

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
#[derive(Debug, Serialize, Deserialize)]
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
#[derive(Debug)]
struct Surface;
impl SurfaceRenderer for Surface {
    fn render(&self, context: &Context) -> Result<UiView> {
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
                    "Workspace review has no editable fields".into(),
                ));
            }
            let operation = serde_json::from_value(input.value).map_err(error)?;
            tokio::select! {biased;
                ()=target.cancelled()=>Err(UiError::Retired),
                ()=target.view_closed()=>Err(UiError::Retired),
                result=read(target.context(),operation)=>result,
            }
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
        workspace: rsi_workspace_protocol::WorkspaceId::parse(hex::encode(Sha256::digest(
            header.canonical_cwd().as_bytes(),
        )))
        .map_err(error)?,
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
        Client::new(api)
            .map_err(error)?
            .call(request)
            .await
            .map_err(error)?,
    ))
}
fn present(reply: Reply) -> UiView {
    let mut elements = vec![home()];
    match reply{
        Reply::Summaries{epoch,items,next}=>{
            elements.push(text("Interval evidence includes concurrent edits, and excludes ignored files and nested repository contents. Complete applies only to the declared text-file scope and controlled work. External agents and detached background work may write after the interval."));
            if items.is_empty(){elements.push(text("No recorded execution intervals."));}
            for item in items{
                let expired=item.epoch!=epoch;
                let state=match(item.phase,expired){(Phase::Pending,true)=>"Interrupted; diff expired",(Phase::Pending,false)=>"Pending; work or capture not settled",(Phase::Complete,false)=>"Complete",(Phase::Complete,true)=>"Complete; diff expired",(Phase::Partial,false)=>"Partial",(Phase::Partial,true)=>"Partial; diff expired"};
                elements.push(UiElement::Field{label:format!("Interval {}",item.id),value:format!("{state} · {} files · +{} / −{} lines · {} → {} ms",item.changed_files,item.added_lines,item.removed_lines,item.started_ms,item.finished_ms.as_deref().unwrap_or("pending"))});
                if !item.omissions.is_empty(){elements.push(text(format!("Coverage omissions: {}",item.omissions.iter().map(|o|format!("{:?}: {}",o.kind,o.count)).collect::<Vec<_>>().join(", "))));}
                if item.phase!=Phase::Pending{elements.push(button("Open interval files",Operation::Files{id:item.id,offset:0}));}
            }
            if let Some(after)=next{elements.push(button("More intervals",Operation::List{after:Some(after)}));}
        }
        Reply::Files{id,offset,files,has_more}=>{
            elements.push(text(format!("Interval {id}. Changes occurred in this interval; concurrent edits may be included.")));
            let shown=files.iter().take(16).scan(0usize, |bytes, file| {*bytes += 2*file.path.len()+file.previous_path.as_ref().map_or(0,String::len)+512; Some(*bytes)}).take_while(|bytes| *bytes <= 16*1024).count();
            if files.is_empty(){elements.push(text("No comparable text changes in this interval."));}
            for file in files.iter().take(shown){
                elements.push(UiElement::Field{label:file.path.clone(),value:format!("+{} / −{}{}",file.added,file.removed,file.previous_path.as_ref().map(|p|format!(" · renamed from {p}")).unwrap_or_default())});
                elements.push(button("Open file diff",Operation::Diff{id:id.clone(),path:file.path.clone(),offset:0}));
            }
            if has_more||shown<files.len(){elements.push(button("More changed files",Operation::Files{id,offset:offset+shown}));}
        }
        Reply::Diff{id,path,offset,has_more,text:content,..}=>{
            let mut end=content.len().min(12*1024);while !content.is_char_boundary(end){end-=1;}
            elements.push(button("Back to interval files",Operation::Files{id:id.clone(),offset:0}));
            elements.push(UiElement::Field{label:"File diff".into(),value:path.clone()});
            elements.push(UiElement::Code{text:content[..end].into()});
            if has_more||end<content.len(){elements.push(button("Continue diff",Operation::Diff{id,path,offset:offset+end}));}
        }
        Reply::Expired{..}=>elements.push(text("Diff expired. The durable summary remains available; comparison content ended with its runtime or was evicted by the scratch limit.")),
    }
    view(elements)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rsi_workspace_review_api::FileChange;
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
            result
                .elements
                .iter()
                .any(|e| matches!(e,UiElement::Text{text} if text.starts_with("Diff expired")))
        );
    }
}
