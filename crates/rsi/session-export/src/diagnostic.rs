use super::{
    Arc, Cut, Interval, Result, SessionFact, SessionId, SessionStore, StreamExt, Value, encoding,
    facts, invalid, json, store_error,
};
use rsi_agent_context::{
    ContextInit, ContextLimits, ContextPage, DefaultContextBuilder, ModelContextBuilder,
};
use rsi_agent_session_protocol::{
    EffectId, EvidencePart, ModelEventPurpose, RequestEvidence, SessionFactBody,
};
use rsi_agent_store_protocol::EvidenceOriginal;
use rsi_ai_protocol::{
    LanguageEvent, LanguageRequestOptions, Message, MessageContent, MessageRole,
};

pub(super) struct Latest {
    pub id: SessionId,
    pub effect: EffectId,
    pub intent: u64,
    pub end: u64,
}

pub(super) async fn latest(cut: &Cut) -> Result<Option<Latest>> {
    for interval in cut.intervals.iter().rev() {
        let mut before = interval
            .through
            .checked_add(1)
            .ok_or_else(|| invalid("export cursor overflow"))?;
        let mut completed = None;
        while before > interval.after + 1 {
            let page = cut
                .store()
                .read_facts_before(&interval.id, before, 32)
                .await
                .map_err(store_error)?;
            if page.facts.is_empty() {
                return Err(invalid("export diagnostic history is incomplete"));
            }
            for fact in page
                .facts
                .iter()
                .rev()
                .take_while(|fact| fact.seq() > interval.after)
            {
                before = fact.seq();
                match fact.body() {
                    SessionFactBody::ModelEvent {
                        effect_id,
                        event: LanguageEvent::Finished { .. },
                        purpose: ModelEventPurpose::Conversation,
                        ..
                    } if completed.is_none() => completed = Some((effect_id.clone(), fact.seq())),
                    SessionFactBody::ModelIntent { effect_id, .. }
                        if completed.as_ref().is_some_and(|(id, _)| id == effect_id) =>
                    {
                        let (effect, end) = completed.expect("matched completion");
                        return Ok(Some(Latest {
                            id: interval.id.clone(),
                            effect,
                            intent: fact.seq(),
                            end,
                        }));
                    }
                    _ => {}
                }
            }
            if page
                .facts
                .first()
                .is_some_and(|fact| fact.seq() <= interval.after)
            {
                break;
            }
        }
        if completed.is_some() {
            return Err(invalid("completed export response has no intent"));
        }
    }
    Ok(None)
}

pub(super) async fn evidence_record(
    store: &dyn SessionStore,
    id: &SessionId,
    fact: &SessionFact,
) -> Result<Option<Value>> {
    let SessionFactBody::ModelIntent {
        evidence,
        effect_id,
        purpose,
        snapshot,
        ..
    } = fact.body()
    else {
        return Ok(None);
    };
    let evidence = resolve(store, id, evidence).await?;
    Ok(Some(
        json!({"session_id":id,"seq":fact.seq().to_string(),"effect_id":effect_id,"purpose":purpose,"snapshot":snapshot,"evidence":evidence}),
    ))
}
async fn resolve(
    store: &dyn SessionStore,
    id: &SessionId,
    evidence: &RequestEvidence,
) -> Result<Value> {
    let RequestEvidence::Available { manifest, .. } = evidence else {
        return serde_json::to_value(evidence).map_err(encoding);
    };
    let mut value = json!({"availability":"available","manifest":manifest});
    for (section, part) in evidence.parts() {
        let key = serde_json::to_value(section)
            .map_err(encoding)?
            .as_str()
            .expect("section string")
            .to_string();
        let text: Result<String> = match part {
            EvidencePart::Inline { text, .. } => Ok(text.clone()),
            EvidencePart::Reference {
                seq,
                section,
                sha256,
                bytes,
            } => match EvidenceOriginal::read(store, id, *seq).await {
                Ok(original) => original
                    .text(*section, sha256, *bytes)
                    .map(str::to_owned)
                    .map_err(encoding),
                Err(error) => Err(encoding(error)),
            },
        };
        // Evidence is opaque exact UTF-8. JSON parsing is needed only by reconstruction.
        if let Ok(text) = text {
            value[&key] = json!({"text":text,"sha256":part.sha256(),"bytes":part.bytes()});
        } else {
            value["availability"] = json!("unavailable");
            value["reason"] = json!("evidence_reference_invalid_or_missing");
            value[&key] = json!({"availability":"unavailable","reference":part});
        }
    }
    Ok(value)
}

pub(super) async fn request(cut: &Cut, latest: &Latest) -> Result<Value> {
    let mut page = cut
        .store()
        .read_facts(&latest.id, latest.intent - 1, 1)
        .await
        .map_err(store_error)?;
    let fact = page
        .facts
        .pop()
        .filter(|f| f.seq() == latest.intent)
        .ok_or_else(|| invalid("export request intent missing"))?;
    let SessionFactBody::ModelIntent {
        evidence, snapshot, ..
    } = fact.body()
    else {
        return Err(invalid("export request source changed"));
    };
    let evidence = resolve(&*cut.store(), &latest.id, evidence).await?;
    let mut result = json!({"raw":false,"reconstructed":true,"approximate":true,"session_id":latest.id,"intent_seq":latest.intent.to_string(),"effect_id":latest.effect,"snapshot":snapshot,
        "reconstructor":DefaultContextBuilder::default().identity(),"projection_limits":ContextLimits::default(),
        "warnings":["Reconstructed with the current default context builder; historical custom builders, context limits and provider transformations are not recorded. Default-context pruning and omission can differ from historical input. Recorded system/developer messages are placed before the reconstructed conversation; historical interleaving may differ.","Media are immutable references, not original provider payload bytes."],"evidence":evidence});
    if result["evidence"]["availability"] != "available" {
        result["availability"] = json!("unavailable");
        result["reason"] = json!("request_evidence_unavailable");
        return Ok(result);
    }
    match reconstruct(cut, latest, &result["evidence"]).await {
        Ok(request) => {
            result["availability"] = json!("available");
            result["request"] = request;
        }
        Err(error) => {
            result["availability"] = json!("unavailable");
            result["reason"] = json!("semantic_reconstruction_unavailable");
            let detail: String = error.to_string().chars().take(1024).collect();
            result["detail"] = json!(detail);
        }
    }
    Ok(result)
}

async fn reconstruct(cut: &Cut, latest: &Latest, evidence: &Value) -> Result<Value> {
    if latest.id != *cut.header.session_id()
        && cut
            .intervals
            .iter()
            .find(|interval| interval.id == latest.id)
            .is_none_or(|interval| interval.after != 0)
    {
        return Err(invalid(
            "inherited request requires history before the selected parent interval",
        ));
    }
    let header = if latest.id == *cut.header.session_id() {
        cut.header.clone()
    } else {
        cut.store().header(&latest.id).await.map_err(store_error)?
    };
    let builder = DefaultContextBuilder::default();
    let mut cursor = builder
        .open(ContextInit {
            identity: builder.identity(),
            header: header.clone(),
            limits: ContextLimits::default(),
            checkpoint: None,
        })
        .map_err(encoding)?;
    // Only the exported Session's proven direct-parent interval may be consulted.
    if let Some(origin) = header.fork_origin() {
        if latest.id != *cut.header.session_id() {
            return Err(invalid(
                "inherited request requires an ancestor outside the selected export",
            ));
        }
        let mut source = facts(
            cut.store(),
            Interval {
                id: origin.parent_session_id.clone(),
                after: origin.resolved_after_seq,
                through: origin.resolved_terminal_seq,
            },
        );
        while let Some(fact) = source.next().await {
            cursor
                .ingest(ContextPage::ForkSeed(&[Arc::new(fact?)]))
                .map_err(encoding)?;
        }
        cursor.ingest(ContextPage::FinishSeed).map_err(encoding)?;
    }
    let mut source = facts(
        cut.store(),
        Interval {
            id: latest.id.clone(),
            after: 0,
            through: latest.intent - 1,
        },
    );
    while let Some(fact) = source.next().await {
        cursor
            .ingest(ContextPage::Canonical(&[Arc::new(fact?)]))
            .map_err(encoding)?;
    }
    let options = recorded_options(evidence)?;
    let projected = cursor.build(options).map_err(encoding)?;
    let mut messages: Vec<Message> = serde_json::from_str(
        evidence["system"]["text"]
            .as_str()
            .ok_or_else(|| invalid("missing request system"))?,
    )
    .map_err(encoding)?;
    for message in projected
        .messages()
        .iter()
        .filter(|message| !matches!(message.role(), MessageRole::System | MessageRole::Developer))
    {
        let content = message
            .content()
            .iter()
            .map(|item| match item {
                MessageContent::Reasoning { text, .. } => MessageContent::Reasoning {
                    text: text.clone(),
                    evidence: None,
                },
                other => other.clone(),
            })
            .collect();
        messages.push(if message.role() == MessageRole::Assistant {
            Message::assistant(content).map_err(encoding)?
        } else {
            message.clone()
        });
    }
    let mut request = serde_json::to_value(projected).map_err(encoding)?;
    request["messages"] = serde_json::to_value(messages).map_err(encoding)?;
    let request: rsi_ai_protocol::LanguageRequest =
        serde_json::from_value(request).map_err(encoding)?;
    serde_json::to_value(request).map_err(encoding)
}

fn recorded_options(evidence: &Value) -> Result<LanguageRequestOptions> {
    let configuration: Value = serde_json::from_str(
        evidence["configuration"]["text"]
            .as_str()
            .ok_or_else(|| invalid("missing request configuration"))?,
    )
    .map_err(encoding)?;
    let tools: Value = serde_json::from_str(
        evidence["tools"]["text"]
            .as_str()
            .ok_or_else(|| invalid("missing request tools"))?,
    )
    .map_err(encoding)?;
    LanguageRequestOptions::new(
        serde_json::from_value(tools["definitions"].clone()).map_err(encoding)?,
        serde_json::from_value(tools["choice"].clone()).map_err(encoding)?,
        serde_json::from_value(tools["hosted"].clone()).map_err(encoding)?,
        serde_json::from_value(configuration["response_format"].clone()).map_err(encoding)?,
        serde_json::from_value(configuration["settings"].clone()).map_err(encoding)?,
        serde_json::from_value(configuration["extensions"].clone()).map_err(encoding)?,
    )
    .map_err(encoding)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rsi_agent_session_protocol::EvidenceSection;
    #[tokio::test]
    async fn corrupt_reference_keeps_valid_sections_and_never_reconstructs_fabricated_input() {
        let store = rsi_agent_testkit::MemoryStore::new();
        let evidence = RequestEvidence::Available {
            configuration: EvidencePart::inline("{}".into()),
            system: EvidencePart::Reference {
                seq: 1,
                section: EvidenceSection::System,
                sha256: "a".repeat(64),
                bytes: 4,
            },
            tools: EvidencePart::inline("{}".into()),
            manifest: vec![],
        };
        let value = resolve(
            &store,
            &SessionId::new("missing-source").unwrap(),
            &evidence,
        )
        .await
        .unwrap();
        assert_eq!(value["availability"], "unavailable");
        assert_eq!(value["configuration"]["text"], "{}");
        assert_eq!(value["tools"]["text"], "{}");
        assert_eq!(value["system"]["availability"], "unavailable");
        assert!(value["system"].get("text").is_none());
    }
    #[tokio::test]
    async fn referenced_sections_require_both_original_digest_and_length() {
        use crate::tests::{accepted, append, evidence, header, intent};
        let store = rsi_agent_testkit::MemoryStore::new();
        let header = header();
        let original = evidence();
        append(
            &store,
            &header,
            0,
            vec![accepted("original"), intent("original", original.clone())],
        )
        .await;
        let original_part = original.part(EvidenceSection::System).unwrap();
        let mut referenced = original.clone();
        let RequestEvidence::Available { system, .. } = &mut referenced else {
            unreachable!()
        };
        *system = EvidencePart::Reference {
            seq: 2,
            section: EvidenceSection::System,
            sha256: original_part.sha256().into(),
            bytes: original_part.bytes().try_into().unwrap(),
        };
        let resolved = resolve(&store, header.session_id(), &referenced)
            .await
            .unwrap();
        assert_eq!(resolved["availability"], "available");
        assert_eq!(
            resolved["system"]["text"],
            resolve(&store, header.session_id(), &original)
                .await
                .unwrap()["system"]["text"]
        );
        for corrupt_digest in [true, false] {
            let mut corrupt = referenced.clone();
            let RequestEvidence::Available {
                system: EvidencePart::Reference { sha256, bytes, .. },
                ..
            } = &mut corrupt
            else {
                unreachable!()
            };
            if corrupt_digest {
                *sha256 = "f".repeat(64);
            } else {
                *bytes += 1;
            }
            let resolved = resolve(&store, header.session_id(), &corrupt)
                .await
                .unwrap();
            assert_eq!(resolved["availability"], "unavailable");
            assert_eq!(resolved["reason"], "evidence_reference_invalid_or_missing");
            assert!(resolved["system"].get("text").is_none());
            assert!(resolved["configuration"].get("text").is_some());
        }
    }
}
