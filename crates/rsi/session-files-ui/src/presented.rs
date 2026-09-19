use crate::{
    FilesBrowserContract,
    browser::{Operation, Request},
};
use futures_util::future::BoxFuture;
use rsi_agent_session_protocol::{SessionFact, SessionFactBody};
use rsi_client::SessionControllerContract;
use rsi_conversation::{BlockIdentity, FactField, SourceRef, ToolValuePath};
use rsi_files_protocol::RelativePath;
use rsi_files_tools::{MAXIMUM_PRESENTED_FILES_BYTES, PresentedFilesV1};
use rsi_meta::Context;
use rsi_ui::{
    ActionInput, ActionTarget, BlockInput, BlockRenderer, Result, SurfaceRenderer, UiAction,
    UiElement, UiError, UiModel, UiView,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Entry {
    pub intent: SourceRef,
    pub result: SourceRef,
    pub index: usize,
}
impl Entry {
    pub(crate) fn validate(&self) -> Result<()> {
        if self.intent.field != FactField::ToolArguments
            || self.result.field != FactField::ToolValue
            || self.intent.seq == 0
            || self.intent.seq >= self.result.seq
            || self.result.seq == u64::MAX
            || self.index >= 8
        {
            return Err(unavailable());
        }
        Ok(())
    }
}
fn unavailable() -> UiError {
    UiError::Invalid("This recorded file declaration is unavailable".into())
}

pub(crate) fn resolve(
    entry: &Entry,
    intents: &[SessionFact],
    results: &[SessionFact],
) -> Result<RelativePath> {
    entry.validate()?;
    let [intent] = intents else {
        return Err(unavailable());
    };
    let [result] = results else {
        return Err(unavailable());
    };
    if intent.seq() != entry.intent.seq || result.seq() != entry.result.seq {
        return Err(unavailable());
    }
    if !matches!(intent.body(), SessionFactBody::ToolIntent { name, .. } if name == "present") {
        return Err(unavailable());
    }
    let SessionFactBody::ToolResult { result: value, .. } = result.body() else {
        return Err(unavailable());
    };
    if value.is_error
        || BlockIdentity::tool(intent).map(BlockIdentity::key)
            != BlockIdentity::tool(result).map(BlockIdentity::key)
    {
        return Err(unavailable());
    }
    let files = PresentedFilesV1::decode(value.value.get("presented").ok_or_else(unavailable)?)
        .map_err(|_| unavailable())?;
    files
        .files
        .get(entry.index)
        .map(|file| file.path_hex.clone())
        .ok_or_else(unavailable)
}

#[derive(Debug)]
pub(crate) struct Renderer;
impl BlockRenderer for Renderer {
    fn render(&self, _: &Context, _: &BlockInput<'_>) -> Result<Option<UiView>> {
        Ok(None)
    }
    fn inline(
        &self,
        _: &Context,
        block: &BlockInput<'_>,
    ) -> Result<Option<Arc<dyn SurfaceRenderer>>> {
        let Some(tool) = block
            .tool
            .filter(|tool| tool.name.as_deref() == Some("present"))
        else {
            return Ok(None);
        };
        let (Some(intent), Some(result)) = (tool.arguments, tool.result) else {
            return Ok(None);
        };
        let entry = Entry {
            intent,
            result,
            index: 0,
        };
        entry.validate()?;
        Ok(Some(Arc::new(Card(entry))))
    }
}
#[derive(Debug)]
struct Card(Entry);
impl SurfaceRenderer for Card {
    fn model(&self, target: Context) -> BoxFuture<'_, Result<UiModel>> {
        Box::pin(async move {
            let controller = target
                .lookup_local::<SessionControllerContract>()
                .ok_or(UiError::Retired)?;
            let stop = CancellationToken::new();
            let _cancel = stop.clone().drop_guard();
            let window = controller
                .tool_value_window(
                    self.0.result,
                    ToolValuePath::new(vec!["presented".into()]).expect("fixed path"),
                    0,
                    MAXIMUM_PRESENTED_FILES_BYTES + 4096,
                    stop,
                )
                .await;
            let files = window
                .ok()
                .filter(|window| !window.more)
                .and_then(|window| serde_json::from_str::<PresentedFilesV1>(&window.text).ok())
                .filter(|files| files.validate().is_ok());
            let Some(files) = files else {
                return Ok(UiModel::standard(UiView {
                    title: "Presented files".into(),
                    elements: vec![UiElement::Text {
                        text: "No supported file declaration is available in this result.".into(),
                    }],
                })?);
            };
            let mut elements = vec![UiElement::Text { text: "Recorded file declaration. Open reads current contents with this Session’s file access.".into() }];
            for (index, file) in files.files.iter().enumerate() {
                let name = crate::view::preview(file.path_hex.as_bytes(), 1024);
                elements.push(UiElement::Field {
                    label: name,
                    value: format!("{} bytes · {}", file.length, file.description),
                });
                let mut entry = self.0.clone();
                entry.index = index;
                elements.push(UiElement::Button {
                    action: "presented_open".into(),
                    label: "Open current file".into(),
                    value: serde_json::to_value(entry).expect("bounded coordinates"),
                });
            }
            Ok(UiModel::standard(UiView {
                title: "Presented files".into(),
                elements,
            })?)
        })
    }
}
#[derive(Debug)]
pub(crate) struct Open;
impl UiAction for Open {
    fn invoke(
        &self,
        target: ActionTarget,
        input: ActionInput,
    ) -> BoxFuture<'static, Result<UiView>> {
        Box::pin(async move {
            if !input.fields.is_empty() {
                return Err(unavailable());
            }
            let entry: Entry = serde_json::from_value(input.value).map_err(|_| unavailable())?;
            entry.validate()?;
            let browser = target
                .context()
                .lookup_local::<FilesBrowserContract>()
                .ok_or(UiError::Retired)?;
            let revision = browser
                .state
                .lock()
                .expect("Files browser state poisoned")
                .revision
                .to_string();
            browser
                .invoke(
                    target,
                    ActionInput {
                        value: serde_json::to_value(Request {
                            revision,
                            operation: Operation::Presented { entry },
                        })
                        .expect("bounded coordinates"),
                        fields: std::collections::BTreeMap::default(),
                    },
                )
                .await
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rsi_agent_session_protocol::{EffectId, TurnId};
    use rsi_tools_protocol::{ToolResult, ToolResultIdentity};
    use serde_json::json;
    fn identity(owner: &str, invoke: &str, call: &str, digest: char) -> ToolResultIdentity {
        ToolResultIdentity::new(owner, invoke, call, digest.to_string().repeat(64)).unwrap()
    }
    fn result(
        seq: u64,
        turn: &str,
        effect: &str,
        identity: ToolResultIdentity,
        version: u32,
        failed: bool,
    ) -> SessionFact {
        SessionFact::new(seq, 1, SessionFactBody::ToolResult {
            turn_id: TurnId::new(turn).unwrap(), effect_id: EffectId::new(effect).unwrap(), identity,
            result: ToolResult::new(json!({"presented":{"version":version,"files":[{"path_hex":"7265706f7274","description":"report","length":42}]}}), vec![], failed).unwrap(),
            conclusion: None,
        }).unwrap()
    }
    #[test]
    #[expect(
        clippy::too_many_lines,
        reason = "One recorded identity scenario changes each coordinate independently"
    )]
    fn recorded_coordinates_require_exact_sequence_name_full_identity_and_supported_success() {
        let intent = SessionFact::new(
            3,
            1,
            SessionFactBody::ToolIntent {
                turn_id: TurnId::new("turn").unwrap(),
                effect_id: EffectId::new("effect").unwrap(),
                source_model_effect_id: EffectId::new("model").unwrap(),
                identity: identity("owner", "invoke", "call", 'a'),
                name: "present".into(),
                arguments: json!({"files":[{"path":"report"}]}),
                approval: None,
                parallel_safe: true,
            },
        )
        .unwrap();
        let entry = Entry {
            intent: SourceRef {
                seq: 3,
                field: FactField::ToolArguments,
            },
            result: SourceRef {
                seq: 5,
                field: FactField::ToolValue,
            },
            index: 0,
        };
        let valid = result(
            5,
            "turn",
            "effect",
            identity("owner", "invoke", "call", 'a'),
            1,
            false,
        );
        assert_eq!(
            resolve(
                &entry,
                std::slice::from_ref(&intent),
                std::slice::from_ref(&valid)
            )
            .unwrap()
            .as_bytes(),
            b"report"
        );
        for invalid in [
            result(
                6,
                "turn",
                "effect",
                identity("owner", "invoke", "call", 'a'),
                1,
                false,
            ),
            result(
                5,
                "other",
                "effect",
                identity("owner", "invoke", "call", 'a'),
                1,
                false,
            ),
            result(
                5,
                "turn",
                "other",
                identity("owner", "invoke", "call", 'a'),
                1,
                false,
            ),
            result(
                5,
                "turn",
                "effect",
                identity("other", "invoke", "call", 'a'),
                1,
                false,
            ),
            result(
                5,
                "turn",
                "effect",
                identity("owner", "other", "call", 'a'),
                1,
                false,
            ),
            result(
                5,
                "turn",
                "effect",
                identity("owner", "invoke", "other", 'a'),
                1,
                false,
            ),
            result(
                5,
                "turn",
                "effect",
                identity("owner", "invoke", "call", 'b'),
                1,
                false,
            ),
            result(
                5,
                "turn",
                "effect",
                identity("owner", "invoke", "call", 'a'),
                2,
                false,
            ),
            result(
                5,
                "turn",
                "effect",
                identity("owner", "invoke", "call", 'a'),
                1,
                true,
            ),
        ] {
            assert!(resolve(&entry, std::slice::from_ref(&intent), &[invalid]).is_err());
        }
        let mut wrong_name = intent.body().clone();
        if let SessionFactBody::ToolIntent { name, .. } = &mut wrong_name {
            *name = "other".into();
        }
        assert!(
            resolve(
                &entry,
                &[SessionFact::new(3, 1, wrong_name).unwrap()],
                std::slice::from_ref(&valid)
            )
            .is_err()
        );
        for index in [1, 8, usize::MAX] {
            assert!(
                resolve(
                    &Entry {
                        index,
                        ..entry.clone()
                    },
                    std::slice::from_ref(&intent),
                    std::slice::from_ref(&valid)
                )
                .is_err()
            );
        }
        assert!(resolve(&entry, &[], &[valid]).is_err());
        let mut invalid = entry;
        invalid.result.seq = u64::MAX;
        assert!(invalid.validate().is_err());
    }
}
