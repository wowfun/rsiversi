use super::{
    ActionInput, ActionTarget, BoxFuture, Context, Deserialize, FieldWindow, Result,
    SOURCE_PAGE_BYTES, Serialize, SourceRef, UiAction, UiElement, UiError, UiView, controller,
};
use futures_util::StreamExt;
use rsi_agent_composition_protocol::ToolOutputCatalog;
use rsi_agent_session_protocol::{AgentControlRecordBody, SessionFact, SessionFactBody};
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
    let stream = session
        .observe(ObservationCursor {
            control_seq: 0,
            fact_seq: entry.result.seq,
        })
        .await
        .map_err(|_| unavailable())?;
    let catalog = catalog_at_intent(stream, entry.intent.seq).await;
    Ok(view(name, result, catalog))
}

async fn catalog_at_intent(
    mut stream: rsi_session_protocol::SessionObservationStream,
    intent_seq: u64,
) -> std::result::Result<Option<ToolOutputCatalog>, String> {
    let mut catalog = Err("missing generation".into());
    while let Some(update) = stream.next().await {
        let SessionObservation::Control {
            record,
            fact_cursor,
            durable_control_seq,
        } = update.map_err(|error| error.to_string())?
        else {
            continue;
        };
        let initial = record.seq() == 1
            && matches!(record.body(), AgentControlRecordBody::DomainStateCommitted { commit } if commit.source().is_initial());
        if fact_cursor >= intent_seq && !initial {
            break;
        }
        if matches!(record.body(), AgentControlRecordBody::DomainStateCommitted { commit } if commit.source().is_generation())
        {
            catalog = generation_catalog(&record, catalog);
        }
        if fact_cursor >= intent_seq || record.seq() == durable_control_seq {
            break;
        }
    }
    catalog
}

fn generation_catalog(
    record: &rsi_agent_session_protocol::AgentControlRecord,
    previous: std::result::Result<Option<ToolOutputCatalog>, String>,
) -> std::result::Result<Option<ToolOutputCatalog>, String> {
    match record.body() {
        AgentControlRecordBody::DomainStateCommitted { commit }
            if commit.source().is_generation() =>
        {
            let id = ToolOutputCatalog::identity();
            if let Some(update) = commit
                .updates()
                .iter()
                .find(|update| update.snapshot().identity().id() == id.id())
            {
                ToolOutputCatalog::from_baseline(std::slice::from_ref(update.snapshot()))
                    .map_err(|error| error.to_string())
            } else if commit.source().is_initial()
                || commit
                    .removed()
                    .iter()
                    .any(|removed| removed.id() == id.id())
            {
                Ok(None)
            } else {
                previous
            }
        }
        _ => Ok(None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rsi_tools_protocol::{ToolOutputDeclaration, ToolResult};
    use serde_json::json;

    #[tokio::test]
    async fn historical_intents_use_their_generation_even_after_later_adoptions() {
        use rsi_agent_session_protocol::{
            AgentControlRecord, DomainMutationSource, DomainRevision, DomainSnapshot,
            DomainStateCommit, DomainStateUpdate, DomainStateValue, GenerationSelection,
        };
        for (intent, expected) in [(4, 1), (12, 2), (24, 3), (34, 3)] {
            let mut records: Vec<_> = (1..=3)
                .map(|version| {
                    let catalog = ToolOutputCatalog::new(std::collections::BTreeMap::from([(
                        "echo".into(),
                        ToolOutputDeclaration::new(
                            "fixture.echo",
                            version,
                            json!({"type":"string"}),
                        )
                        .unwrap(),
                    )]))
                    .unwrap();
                    let source = if version == 1 {
                        DomainMutationSource::Generation {
                            selection: GenerationSelection::new("a".repeat(64)).unwrap(),
                            turn_id: None,
                        }
                    } else {
                        DomainMutationSource::Recovery {
                            selection: GenerationSelection::new("b".repeat(64)).unwrap(),
                        }
                    };
                    let commit = DomainStateCommit::new(
                        None,
                        source,
                        vec![
                            DomainStateUpdate::new(
                                DomainRevision::new(u64::from(version - 1)),
                                DomainSnapshot::new(
                                    ToolOutputCatalog::identity(),
                                    DomainStateValue::new(serde_json::to_value(catalog).unwrap())
                                        .unwrap(),
                                ),
                            )
                            .unwrap(),
                        ],
                    )
                    .unwrap();
                    std::sync::Arc::new(
                        AgentControlRecord::new(
                            u64::from(version),
                            1,
                            AgentControlRecordBody::DomainStateCommitted { commit },
                        )
                        .unwrap(),
                    )
                })
                .collect();
            records.push(std::sync::Arc::new(
                AgentControlRecord::new(
                    4,
                    1,
                    AgentControlRecordBody::DomainStateCommitted {
                        commit: DomainStateCommit::new(
                            None,
                            DomainMutationSource::Recovery {
                                selection: GenerationSelection::new("c".repeat(64)).unwrap(),
                            },
                            vec![],
                        )
                        .unwrap(),
                    },
                )
                .unwrap(),
            ));
            let retained = rsi_agent_turn_protocol::ObservationRetention::default()
                .retain_controls(records)
                .unwrap();
            let observations =
                retained
                    .into_iter()
                    .zip([7, 8, 20, 30])
                    .map(|(record, fact_cursor)| {
                        Ok(SessionObservation::Control {
                            record,
                            fact_cursor,
                            durable_control_seq: 4,
                        })
                    });
            let catalog =
                catalog_at_intent(Box::pin(futures_util::stream::iter(observations)), intent)
                    .await
                    .unwrap()
                    .unwrap();
            assert_eq!(catalog.get("echo").unwrap().version(), expected);
        }
    }

    #[test]
    fn fresh_generation_control_supplies_the_recorded_output_contract() {
        use rsi_agent_session_protocol::{
            AgentControlRecord, DomainMutationSource, DomainRevision, DomainSnapshot,
            DomainStateCommit, DomainStateUpdate, DomainStateValue, GenerationSelection,
        };
        let catalog = ToolOutputCatalog::new(std::collections::BTreeMap::from([(
            "echo".into(),
            ToolOutputDeclaration::new("fixture.echo", 1, json!({"type":"string"})).unwrap(),
        )]))
        .unwrap();
        let snapshot = DomainSnapshot::new(
            ToolOutputCatalog::identity(),
            DomainStateValue::new(serde_json::to_value(&catalog).unwrap()).unwrap(),
        );
        let record = AgentControlRecord::new(
            1,
            1,
            AgentControlRecordBody::DomainStateCommitted {
                commit: DomainStateCommit::new(
                    None,
                    DomainMutationSource::Generation {
                        selection: GenerationSelection::new("a".repeat(64)).unwrap(),
                        turn_id: None,
                    },
                    vec![DomainStateUpdate::new(DomainRevision::new(0), snapshot).unwrap()],
                )
                .unwrap(),
            },
        )
        .unwrap();
        assert_eq!(
            generation_catalog(&record, Ok(None)).unwrap(),
            Some(catalog)
        );
    }

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
