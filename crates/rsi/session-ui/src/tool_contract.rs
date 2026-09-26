use super::{
    ActionInput, ActionTarget, BoxFuture, Context, Deserialize, FieldWindow, Result,
    SOURCE_PAGE_BYTES, Serialize, SourceRef, UiAction, UiElement, UiError, UiView, controller,
};
use futures_util::StreamExt;
use rsi_agent_composition_protocol::ToolOutputCatalog;
use rsi_agent_session_protocol::{
    AgentControlRecordBody, DomainMutationSource, SessionFact, SessionFactBody,
};
use rsi_agent_turn_protocol::{ObservationCursor, SessionObservation};
use rsi_conversation::{BlockIdentity, FactField};

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Entry {
    intent: SourceRef,
    result: SourceRef,
}

pub(super) fn button(intent: SourceRef, result: SourceRef) -> UiElement {
    UiElement::Button {
        action: "tool-contract".into(),
        label: "Recorded result".into(),
        value: serde_json::to_value(Entry { intent, result }).expect("source coordinates"),
    }
}
fn unavailable() -> UiError {
    UiError::Invalid("Recorded tool result is unavailable".into())
}

fn resolve<'a>(
    entry: &Entry,
    intent: &'a [SessionFact],
    result: &'a [SessionFact],
) -> Result<(&'a str, &'a rsi_tools_protocol::ToolResult)> {
    let ([intent], [result]) = (intent, result) else {
        return Err(unavailable());
    };
    if entry.intent.field != FactField::ToolArguments
        || entry.result.field != FactField::ToolValue
        || entry.intent.seq == 0
        || entry.intent.seq >= entry.result.seq
        || intent.seq() != entry.intent.seq
        || result.seq() != entry.result.seq
        || BlockIdentity::tool(intent).map(BlockIdentity::key)
            != BlockIdentity::tool(result).map(BlockIdentity::key)
    {
        return Err(unavailable());
    }
    let SessionFactBody::ToolIntent { name, .. } = intent.body() else {
        return Err(unavailable());
    };
    let SessionFactBody::ToolResult { result, .. } = result.body() else {
        return Err(unavailable());
    };
    Ok((name, result))
}

fn view(
    name: &str,
    result: &rsi_tools_protocol::ToolResult,
    catalog: std::result::Result<Option<ToolOutputCatalog>, String>,
) -> UiView {
    let mut elements = Vec::new();
    match catalog {
        Ok(Some(catalog)) if !result.is_error => match catalog.get(name) {
            Some(output) if output.validate_value(&result.value).is_ok() => {
                elements.push(UiElement::Field {
                    label: "Result contract".into(),
                    value: format!("{} · version {}", output.contract_id(), output.version()),
                });
                if let Some(properties) = output
                    .schema()
                    .get("properties")
                    .and_then(serde_json::Value::as_object)
                {
                    for (key, schema) in properties.iter().take(16) {
                        if let Some(value) = result.value.get(key) {
                            let label = schema
                                .get("title")
                                .and_then(serde_json::Value::as_str)
                                .unwrap_or(key);
                            elements.push(UiElement::Field {
                                label: rsi_tools_protocol::safe_tool_text(
                                    FieldWindow::text(label, 0, 128)
                                        .expect("bounded label")
                                        .text
                                        .as_bytes(),
                                ),
                                value: FieldWindow::text(&value.to_string(), 0, 1024)
                                    .expect("bounded field")
                                    .text,
                            });
                        }
                    }
                }
            }
            Some(_) => elements.push(UiElement::Text {
                text: "The recorded value does not match its saved contract. Raw result follows."
                    .into(),
            }),
            None => elements.push(UiElement::Text {
                text: "Opaque JSON · no recorded output declaration.".into(),
            }),
        },
        Ok(_) => elements.push(UiElement::Text {
            text: if result.is_error {
                "Tool reported an error. Raw result follows."
            } else {
                "Opaque JSON · no recorded output declaration."
            }
            .into(),
        }),
        Err(_) => elements.push(UiElement::Text {
            text: "The recorded output declaration is unavailable. Raw result follows.".into(),
        }),
    }
    let window = FieldWindow::text(&result.value.to_string(), 0, SOURCE_PAGE_BYTES)
        .expect("bounded canonical preview");
    elements.push(UiElement::Code { text: window.text });
    if window.more {
        elements.push(UiElement::Text {
            text: "Preview shortened. Use Result in Tool details for complete pages.".into(),
        });
    }
    UiView {
        title: "Recorded tool result".into(),
        elements,
    }
}

async fn recorded(target: &Context, entry: Entry) -> Result<UiView> {
    if entry.result.seq == u64::MAX
        || entry.intent.seq == 0
        || entry.intent.seq >= entry.result.seq
        || entry.intent.field != FactField::ToolArguments
        || entry.result.field != FactField::ToolValue
    {
        return Err(unavailable());
    }
    let controller = controller(target)?;
    let session = target
        .lookup_local::<rsi_session_protocol::SessionContract>()
        .ok_or(UiError::Retired)?
        .attach(controller.session_id())
        .await
        .map_err(|error| UiError::Action(error.to_string()))?;
    let intent = session
        .history_before(Some(entry.intent.seq + 1), 1)
        .await
        .map_err(|_| unavailable())?;
    let result = session
        .history_before(Some(entry.result.seq + 1), 1)
        .await
        .map_err(|_| unavailable())?;
    let (name, result) = resolve(&entry, &intent.facts, &result.facts)?;
    let mut stream = session
        .observe(ObservationCursor::default())
        .await
        .map_err(|_| unavailable())?;
    let catalog = match stream.next().await {
        Some(Ok(SessionObservation::Control { record, .. })) if record.seq() == 1 => {
            match record.body() {
                AgentControlRecordBody::DomainStateCommitted { commit }
                    if matches!(commit.source(), DomainMutationSource::Baseline) =>
                {
                    let baseline = commit
                        .updates()
                        .iter()
                        .map(|update| update.snapshot().clone())
                        .collect::<Vec<_>>();
                    ToolOutputCatalog::from_baseline(&baseline).map_err(|error| error.to_string())
                }
                _ => Ok(None),
            }
        }
        _ => Err("missing baseline".into()),
    };
    Ok(view(name, result, catalog))
}

#[cfg(test)]
mod tests {
    use super::*;
    use rsi_tools_protocol::{ToolOutputDeclaration, ToolResult};
    use serde_json::json;

    #[test]
    fn recorded_pair_rejects_other_effect_owner_turn_and_wrong_coordinates() {
        use rsi_agent_session_protocol::{EffectId, TurnId};
        use rsi_tools_protocol::ToolResultIdentity;
        let intent = SessionFact::new(
            2,
            1,
            SessionFactBody::ToolIntent {
                turn_id: TurnId::new("turn").unwrap(),
                effect_id: EffectId::new("effect").unwrap(),
                origin: rsi_agent_session_protocol::ToolOrigin::Model {
                    effect_id: EffectId::new("model").unwrap(),
                },
                program_role: rsi_tools_protocol::ToolProgramRole::Unavailable,
                identity: ToolResultIdentity::new("owner", "invoke", "call", "a".repeat(64))
                    .unwrap(),
                name: "echo".into(),
                arguments: json!({}),
                approval: None,
                parallel_safe: false,
            },
        )
        .unwrap();
        let entry = Entry {
            intent: SourceRef {
                seq: 2,
                field: FactField::ToolArguments,
            },
            result: SourceRef {
                seq: 4,
                field: FactField::ToolValue,
            },
        };
        for (owner, effect, turn, seq, valid) in [
            ("owner", "effect", "turn", 4, true),
            ("other", "effect", "turn", 4, false),
            ("owner", "other", "turn", 4, false),
            ("owner", "effect", "other", 4, false),
            ("owner", "effect", "turn", 5, false),
        ] {
            let result = SessionFact::new(
                seq,
                1,
                SessionFactBody::ToolResult {
                    turn_id: TurnId::new(turn).unwrap(),
                    effect_id: EffectId::new(effect).unwrap(),
                    identity: ToolResultIdentity::new(owner, "invoke", "call", "a".repeat(64))
                        .unwrap(),
                    result: ToolResult::new(json!({}), vec![], false).unwrap(),
                    conclusion: None,
                },
            )
            .unwrap();
            assert_eq!(
                resolve(&entry, std::slice::from_ref(&intent), &[result]).is_ok(),
                valid
            );
        }
    }

    #[test]
    fn saved_output_projects_typed_fields_and_preserves_raw_fallbacks() {
        let declaration = ToolOutputDeclaration::new("fixture.echo", 1,
            json!({"type":"object","properties":{"label":{"type":"string","title":"Generation label"}},"required":["label"]})).unwrap();
        let catalog = ToolOutputCatalog::new(std::collections::BTreeMap::from([(
            "echo".into(),
            declaration,
        )]))
        .unwrap();
        let result = ToolResult::new(
            json!({"label":"中文 <script>literal</script>"}),
            vec![],
            false,
        )
        .unwrap();
        let card = view("echo", &result, Ok(Some(catalog.clone())));
        card.validate().unwrap();
        assert!(card.elements.iter().any(|element| matches!(element, UiElement::Field { label, value } if label == "Generation label" && value.contains("中文 <script>literal</script>"))));
        for fallback in [Ok(None), Err("unsupported codec".into())] {
            let card = view("echo", &result, fallback);
            card.validate().unwrap();
            assert!(card.elements.iter().any(
                |element| matches!(element, UiElement::Code { text } if text.contains("中文"))
            ));
        }
        let mismatch = ToolResult::new(json!("private scalar"), vec![], false).unwrap();
        let card = view("echo", &mismatch, Ok(Some(catalog.clone())));
        assert!(card.elements.iter().any(
            |element| matches!(element, UiElement::Text { text } if text.contains("does not match"))
        ));
        let error = ToolResult::new(json!("tool failed"), vec![], true).unwrap();
        let card = view("echo", &error, Ok(Some(catalog)));
        assert!(card.elements.iter().any(|element| matches!(element, UiElement::Text { text } if text.contains("reported an error"))));
    }
}

#[derive(Debug)]
pub(super) struct Read;
impl UiAction for Read {
    fn invoke(
        &self,
        target: ActionTarget,
        input: ActionInput,
    ) -> BoxFuture<'static, Result<UiView>> {
        Box::pin(async move {
            if !input.fields.is_empty() {
                return Err(unavailable());
            }
            let entry = serde_json::from_value(input.value).map_err(|_| unavailable())?;
            tokio::select! { biased;
                () = target.cancelled() => Err(UiError::Retired),
                () = target.view_closed() => Err(UiError::Retired),
                result = recorded(target.context(), entry) => result,
            }
        })
    }
}
