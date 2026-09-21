use super::*;
use rsi_agent_store_protocol::StoreFactSuffix;
use rsi_ai_protocol::{ContentDelta, LanguageEvent};

/// Only this module can construct or mutate an admitted capture envelope.
#[derive(Debug, serde::Serialize)]
#[serde(transparent)]
pub(super) struct ValidatedEnvelope(ReferenceSnapshotEnvelope);
impl ValidatedEnvelope {
    pub(super) fn new(envelope: ReferenceSnapshotEnvelope) -> Result<Self> {
        envelope.validate().map_err(invalid)?;
        Ok(Self(envelope))
    }
    pub(super) fn into_frozen(self, snapshot: ReferenceSnapshotRef) -> Result<FrozenReference> {
        let reference = FrozenReference {
            snapshot,
            metadata: self.0.metadata,
            preview: self.0.preview,
        };
        reference.validate().map_err(invalid)?;
        Ok(reference)
    }
}
#[cfg(test)]
impl ValidatedEnvelope {
    fn as_envelope(&self) -> &ReferenceSnapshotEnvelope {
        &self.0
    }
}

#[expect(
    clippy::needless_pass_by_value,
    reason = "Consume the bounded suffix so its Facts drop before CAS publication."
)]
pub(super) fn capture(
    source: &SessionHeader,
    target: &SessionHeader,
    suffix: StoreFactSuffix,
) -> Result<ValidatedEnvelope> {
    suffix.validate(MAXIMUM_REFERENCE_SCAN_FACTS, MAXIMUM_REFERENCE_SCAN_BYTES)?;
    let mut parts = Vec::new();
    let mut remaining = MAXIMUM_REFERENCE_TEXT_BYTES;
    let mut retained_after = suffix.through_seq;
    let mut retained_through = 0;
    let mut content_limited = false;
    for group in groups(&suffix.facts).into_iter().rev() {
        // One bounded label per conversation block, independent of streaming chunk size.
        const LABEL_BYTES: usize = 96;
        if remaining <= LABEL_BYTES + 4 {
            content_limited = true;
            break;
        }
        let mut available = remaining - LABEL_BYTES;
        let mut retained = Vec::new();
        let mut first = 0;
        let mut last = 0;
        for (seq, text) in group.fragments.into_iter().rev() {
            let take = available.min(text.len());
            let start = text.ceil_char_boundary(text.len() - take);
            if start < text.len() {
                let text = &text[start..];
                available -= text.len();
                retained.push(text);
                first = seq;
                last = last.max(seq);
            }
            if start > 0 {
                content_limited = true;
                break;
            }
        }
        if retained.is_empty() {
            content_limited = true;
            break;
        }
        retained.reverse();
        let label = format!("\n[{} · Facts {first}–{last}]\n", group.role);
        debug_assert!(label.len() <= LABEL_BYTES);
        let part = label + &retained.concat();
        remaining -= part.len();
        retained_after = first - 1;
        retained_through = retained_through.max(last);
        parts.push(part);
        if content_limited {
            break;
        }
    }
    if parts.is_empty() {
        return Err(invalid(
            "the bounded source interval contains no exportable conversation text",
        ));
    }
    parts.reverse();
    let text = parts.concat();
    let mut omissions = Vec::new();
    if suffix.after_seq() > 0 {
        omissions.push(if suffix.byte_limited {
            ReferenceOmission::ScanBytes
        } else {
            ReferenceOmission::FactLimit
        });
    }
    if content_limited {
        omissions.push(ReferenceOmission::ContentBytes);
    }
    let envelope = ReferenceSnapshotEnvelope {
        version: 2,
        metadata: ReferenceMetadata {
            source: ReferenceSource::Native {
                binding: ReferenceBinding {
                    session_id: source.session_id().clone(),
                    header_sha256: source.fingerprint().map_err(invalid)?,
                },
            },
            target: ReferenceBinding {
                session_id: target.session_id().clone(),
                header_sha256: target.fingerprint().map_err(invalid)?,
            },
            text_bytes: text.len(),
            capture: ReferenceCapture::Suffix {
                interval: ReferenceSuffix {
                    through_seq: suffix.through_seq,
                    fact_prefix_sha256: suffix.fact_prefix_sha256.clone(),
                    scanned_after_seq: suffix.after_seq(),
                    retained_after_seq: retained_after,
                    retained_through_seq: retained_through,
                    scanned_bytes: suffix.encoded_bytes,
                    omissions,
                },
            },
        },
        preview: text[..text.floor_char_boundary(text.len().min(MAXIMUM_REFERENCE_PREVIEW_BYTES))]
            .into(),
        text,
    };
    ValidatedEnvelope::new(envelope)
}

#[derive(Eq, PartialEq)]
enum Block<'a> {
    Human(u64),
    Assistant(&'a TurnId, &'a EffectId, u32),
}
struct Group<'a> {
    key: Block<'a>,
    role: &'static str,
    fragments: Vec<(u64, &'a str)>,
}
fn groups(facts: &[SessionFact]) -> Vec<Group<'_>> {
    let mut groups: Vec<Group<'_>> = Vec::new();
    for fact in facts {
        match fact.body() {
            SessionFactBody::TurnAccepted { text, .. } => append_group(
                &mut groups,
                Block::Human(fact.seq()),
                "Human",
                fact.seq(),
                text,
            ),
            SessionFactBody::ImageRequested { request, .. } => append_group(
                &mut groups,
                Block::Human(fact.seq()),
                "Human",
                fact.seq(),
                request.prompt(),
            ),
            SessionFactBody::InputMessageEntered {
                source: InputMessageSource::Human { .. },
                content,
                ..
            } => {
                for item in content {
                    if let AgentMessageContent::Text { text } = item {
                        append_group(
                            &mut groups,
                            Block::Human(fact.seq()),
                            "Human",
                            fact.seq(),
                            text,
                        );
                    }
                }
            }
            SessionFactBody::ModelEvent {
                turn_id,
                effect_id,
                purpose: ModelEventPurpose::Conversation,
                event:
                    LanguageEvent::ContentDelta {
                        index,
                        delta: ContentDelta::Text(text),
                    },
                ..
            } => append_group(
                &mut groups,
                Block::Assistant(turn_id, effect_id, *index),
                "Assistant",
                fact.seq(),
                text,
            ),
            _ => {}
        }
    }
    groups
}
fn append_group<'a>(
    groups: &mut Vec<Group<'a>>,
    key: Block<'a>,
    role: &'static str,
    seq: u64,
    text: &'a str,
) {
    if text.is_empty() {
        return;
    }
    if let Some(last) = groups.last_mut()
        && last.key == key
    {
        last.fragments.push((seq, text));
    } else {
        groups.push(Group {
            key,
            role,
            fragments: vec![(seq, text)],
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rsi_agent_session_protocol::{
        AgentPresetId, ContributionId, FrozenAgentSettings, MessageId, StepId,
    };
    fn header() -> SessionHeader {
        SessionHeader::new(
            SessionId::new("test").unwrap(),
            1,
            "/workspace",
            AgentPresetId::new("test").unwrap(),
            FrozenAgentSettings::new(
                "default",
                "",
                rsi_ai_protocol::ModelRef::new("test", "model").unwrap(),
                rsi_sandbox::SandboxMode::ReadOnly,
                false,
            )
            .unwrap(),
        )
        .unwrap()
    }
    fn suffix(after: u64, bodies: Vec<SessionFactBody>) -> StoreFactSuffix {
        let facts: Vec<_> = bodies
            .into_iter()
            .enumerate()
            .map(|(index, body)| SessionFact::new(after + index as u64 + 1, 1, body).unwrap())
            .collect();
        StoreFactSuffix {
            through_seq: after + facts.len() as u64,
            fact_prefix_sha256: "a".repeat(64),
            encoded_bytes: facts.iter().map(SessionFact::encoded_len).sum(),
            facts,
            byte_limited: false,
        }
    }
    fn input(source: InputMessageSource, content: Vec<AgentMessageContent>) -> SessionFactBody {
        SessionFactBody::InputMessageEntered {
            turn_id: TurnId::new("turn").unwrap(),
            step_id: StepId::new("step").unwrap(),
            source,
            content,
        }
    }
    fn text(text: &str) -> Vec<AgentMessageContent> {
        vec![AgentMessageContent::Text { text: text.into() }]
    }
    #[test]
    fn validated_capture_preserves_wire_bytes_and_checks_the_returned_cas_reference() {
        let header = header();
        let captured = || {
            capture(
                &header,
                &header,
                suffix(
                    0,
                    vec![input(
                        InputMessageSource::Human {
                            message_id: MessageId::new("human").unwrap(),
                        },
                        text("visible \\\n界"),
                    )],
                ),
            )
            .unwrap()
        };
        let envelope = captured();
        let bytes = serde_json::to_vec(&envelope).unwrap();
        assert_eq!(bytes, serde_json::to_vec(&envelope.0).unwrap());
        let byte_len = u64::try_from(bytes.len()).unwrap();
        let snapshot = ReferenceSnapshotRef {
            sha256: "a".repeat(64),
            byte_len,
        };
        let expected = envelope.0.frozen(snapshot.clone()).unwrap();
        assert_eq!(envelope.into_frozen(snapshot).unwrap(), expected);
        for (sha256, byte_len) in [(String::new(), byte_len), ("a".repeat(64), 0)] {
            assert!(
                captured()
                    .into_frozen(ReferenceSnapshotRef { sha256, byte_len })
                    .is_err()
            );
        }
    }
    fn model(purpose: ModelEventPurpose, delta: ContentDelta) -> SessionFactBody {
        SessionFactBody::ModelEvent {
            turn_id: TurnId::new("turn").unwrap(),
            effect_id: EffectId::new("effect").unwrap(),
            purpose,
            event: LanguageEvent::ContentDelta { index: 0, delta },
        }
    }
    #[test]
    #[expect(
        clippy::too_many_lines,
        reason = "One sequential public-seam scenario preserves causality and exact evidence"
    )]
    fn export_excludes_reasoning_tools_injected_context_skills_and_nested_references() {
        let header = header();
        let nested = capture(
            &header,
            &header,
            suffix(
                0,
                vec![input(
                    InputMessageSource::Human {
                        message_id: MessageId::new("nested").unwrap(),
                    },
                    text("nested-secret"),
                )],
            ),
        )
        .unwrap()
        .as_envelope()
        .frozen(ReferenceSnapshotRef {
            sha256: "b".repeat(64),
            byte_len: 100,
        })
        .unwrap();
        let exported = capture(
            &header,
            &header,
            suffix(
                0,
                vec![
                    input(
                        InputMessageSource::Human {
                            message_id: MessageId::new("human").unwrap(),
                        },
                        vec![
                            AgentMessageContent::Text {
                                text: "visible-human".into(),
                            },
                            AgentMessageContent::Reference { reference: nested },
                        ],
                    ),
                    input(
                        InputMessageSource::AgentInstructions {
                            source: "instructions".into(),
                            sha256: "a".repeat(64),
                            replacement: false,
                            tombstone: false,
                        },
                        text("instruction-secret"),
                    ),
                    input(
                        InputMessageSource::SkillCatalog {
                            sha256: "a".repeat(64),
                        },
                        text("catalog-secret"),
                    ),
                    input(
                        InputMessageSource::UserSkillInvocation {
                            name: "skill".into(),
                            source: "skills/test/SKILL.md".into(),
                        },
                        text("skill-body-secret"),
                    ),
                    input(
                        InputMessageSource::PluginContext {
                            contribution_id: ContributionId::new("test.context").unwrap(),
                        },
                        text("plugin-secret"),
                    ),
                    input(
                        InputMessageSource::Agent {
                            message_id: MessageId::new("agent").unwrap(),
                            source_session_id: SessionId::new("child").unwrap(),
                        },
                        text("agent-message-secret"),
                    ),
                    model(
                        ModelEventPurpose::Conversation,
                        ContentDelta::Reasoning("reasoning-secret".into()),
                    ),
                    model(
                        ModelEventPurpose::ContextCompaction,
                        ContentDelta::Text("compaction-secret".into()),
                    ),
                    model(
                        ModelEventPurpose::Conversation,
                        ContentDelta::ToolArguments("tool-arguments-secret".into()),
                    ),
                    model(
                        ModelEventPurpose::Conversation,
                        ContentDelta::Text("visible-assistant".into()),
                    ),
                    SessionFactBody::ToolResult {
                        turn_id: TurnId::new("turn").unwrap(),
                        effect_id: EffectId::new("tool").unwrap(),
                        identity: rsi_tools_protocol::ToolResultIdentity::new(
                            "owner",
                            "invoke",
                            "call",
                            "a".repeat(64),
                        )
                        .unwrap(),
                        result: rsi_tools_protocol::ToolResult::new(
                            serde_json::json!({"data":"tool-result-secret"}),
                            vec![rsi_tools_protocol::ToolContent::Text {
                                text: "tool-content-secret".into(),
                            }],
                            false,
                        )
                        .unwrap(),
                        conclusion: None,
                    },
                ],
            ),
        )
        .unwrap();
        assert!(
            exported.as_envelope().text.contains("visible-human")
                && exported.as_envelope().text.contains("visible-assistant")
        );
        assert!(
            !exported.as_envelope().text.contains("secret"),
            "{}",
            exported.as_envelope().text
        );
        assert_eq!(exported.as_envelope().metadata.retained_interval().0, 1);
        assert_eq!(exported.as_envelope().metadata.retained_interval().1, 10);
    }
    #[test]
    fn streaming_deltas_coalesce_without_joining_distinct_model_blocks() {
        let header = header();
        let mut other = model(
            ModelEventPurpose::Conversation,
            ContentDelta::Text("second block".into()),
        );
        if let SessionFactBody::ModelEvent {
            event: LanguageEvent::ContentDelta { index, .. },
            ..
        } = &mut other
        {
            *index = 1;
        }
        let envelope = capture(
            &header,
            &header,
            suffix(
                0,
                vec![
                    model(
                        ModelEventPurpose::Conversation,
                        ContentDelta::Text("Hello ".into()),
                    ),
                    model(
                        ModelEventPurpose::Conversation,
                        ContentDelta::Reasoning("hidden".into()),
                    ),
                    model(
                        ModelEventPurpose::Conversation,
                        ContentDelta::Text("世界".into()),
                    ),
                    other,
                    SessionFactBody::ImageRequested {
                        turn_id: TurnId::new("image").unwrap(),
                        model: rsi_ai_protocol::ModelRef::new("test", "image").unwrap(),
                        request: rsi_ai_protocol::ImageRequest::new(
                            "A visible human image prompt",
                            1,
                        )
                        .unwrap(),
                    },
                ],
            ),
        )
        .unwrap();
        assert!(envelope.as_envelope().text.contains("Hello 世界"));
        assert_eq!(envelope.as_envelope().text.matches("[Assistant").count(), 2);
        assert!(
            envelope
                .as_envelope()
                .text
                .contains("A visible human image prompt")
        );
        assert!(!envelope.as_envelope().text.contains("hidden"));
    }
    #[test]
    fn utf8_content_preview_and_source_coordinates_keep_their_exact_bounds() {
        let header = header();
        let envelope = capture(
            &header,
            &header,
            suffix(
                9_007_199_254_740_992,
                vec![model(
                    ModelEventPurpose::Conversation,
                    ContentDelta::Text("界".repeat(400_000)),
                )],
            ),
        )
        .unwrap();
        assert!(envelope.as_envelope().text.len() <= MAXIMUM_REFERENCE_TEXT_BYTES);
        assert!(envelope.as_envelope().preview.len() <= MAXIMUM_REFERENCE_PREVIEW_BYTES);
        assert!(
            envelope
                .as_envelope()
                .text
                .starts_with(&envelope.as_envelope().preview)
        );
        assert_eq!(
            envelope.as_envelope().metadata.omissions(),
            vec![
                ReferenceOmission::FactLimit,
                ReferenceOmission::ContentBytes
            ]
        );
        let value = serde_json::to_value(&envelope).unwrap();
        assert_eq!(
            value["metadata"]["capture"]["interval"]["through_seq"],
            "9007199254740993"
        );
        let decoded: ReferenceSnapshotEnvelope = serde_json::from_value(value).unwrap();
        decoded.validate().unwrap();
        assert_eq!(decoded, envelope.0);
        let mut invalid = envelope.0;
        invalid.preview.push('x');
        assert!(ValidatedEnvelope::new(invalid).is_err());
        assert!(
            capture(
                &header,
                &header,
                suffix(
                    0,
                    vec![model(
                        ModelEventPurpose::Conversation,
                        ContentDelta::Reasoning("not exported".into())
                    )]
                )
            )
            .is_err()
        );
    }

    #[tokio::test]
    async fn verified_envelopes_reuse_decoding_but_keep_binding_checks_and_two_entry_bound() {
        let header = header();
        let store = rsi_agent_testkit::MemoryStore::default();
        let cache = std::sync::Mutex::new(crate::VerifiedEnvelopes::default());
        let stop = tokio_util::sync::CancellationToken::new();
        let mut saved = Vec::new();
        for text in ["first", "second", "third"] {
            let envelope = capture(
                &header,
                &header,
                suffix(
                    0,
                    vec![model(
                        ModelEventPurpose::Conversation,
                        ContentDelta::Text(text.into()),
                    )],
                ),
            )
            .unwrap();
            let object = store
                .put_cas(serde_json::to_vec(&envelope).unwrap().into())
                .await
                .unwrap();
            let reference = envelope
                .as_envelope()
                .frozen(ReferenceSnapshotRef {
                    sha256: object.sha256,
                    byte_len: object.byte_len,
                })
                .unwrap();
            let first = crate::load(&store, &header, &reference, &stop, &cache)
                .await
                .unwrap();
            for _ in 0..16 {
                let again = crate::load(&store, &header, &reference, &stop, &cache)
                    .await
                    .unwrap();
                assert!(
                    std::sync::Arc::ptr_eq(&first, &again),
                    "cache hits must reuse the decoded envelope"
                );
            }
            let mut tampered = reference.clone();
            tampered.preview = "changed".into();
            assert!(
                crate::load(&store, &header, &tampered, &stop, &cache)
                    .await
                    .is_err()
            );
            tampered = reference.clone();
            tampered.snapshot.byte_len += 1;
            assert!(
                crate::load(&store, &header, &tampered, &stop, &cache)
                    .await
                    .is_err()
            );
            saved.push((reference, first));
            assert!(cache.lock().unwrap().0.len() <= 2);
        }
        let first = crate::load(&store, &header, &saved[0].0, &stop, &cache)
            .await
            .unwrap();
        assert!(
            !std::sync::Arc::ptr_eq(&first, &saved[0].1),
            "oldest entry must be evicted"
        );
        assert_eq!(cache.lock().unwrap().0.len(), 2);
    }

    #[tokio::test]
    async fn reference_worker_panic_remains_distinct_from_caller_cancellation() {
        let runtime = rsi_meta::Runtime::default();
        let owner = crate::References::new(
            std::sync::Arc::new(rsi_agent_testkit::MemoryStore::default()),
            runtime.execution().clone(),
        );
        let result = owner
            .run::<(), _, _>(CancellationToken::new(), |_, _, _| async {
                panic!("injected reference worker failure")
            })
            .await;
        assert!(matches!(result, Err(ReferenceError::WorkerFailed)));
        owner.close().await;
        assert!(runtime.shutdown().await.is_clean());
    }
}
