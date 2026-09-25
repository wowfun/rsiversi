use super::*;
use rsi_agent_session_protocol::*;
use rsi_agent_store_protocol::AppendBatch;
use rsi_agent_testkit::{MemoryStore, append_history_fixture};
use rsi_ai_protocol::*;
use rsi_sandbox::SandboxMode;
use rsi_session_protocol::Result;

#[path = "tests/seams.rs"]
mod seams;

pub(super) fn header() -> SessionHeader {
    SessionHeader::new(
        SessionId::new("export-test").unwrap(),
        1,
        "/workspace",
        AgentPresetId::new("test").unwrap(),
        FrozenAgentSettings::new(
            "default",
            "hidden instructions",
            ModelRef::new("deployment", "model").unwrap(),
            SandboxMode::ReadOnly,
            false,
        )
        .unwrap(),
    )
    .unwrap()
}
fn turn() -> TurnId {
    TurnId::new("turn").unwrap()
}
pub(super) fn accepted(text: &str) -> SessionFactBody {
    SessionFactBody::TurnAccepted {
        turn_id: turn(),
        text: text.into(),
        model: None,
        reasoning_effort: None,
        sandbox: SandboxMode::ReadOnly,
        require_approval: false,
    }
}
fn snapshot() -> PreparedCallSnapshot {
    PreparedCallSnapshot {
        language_settings: None,
        call_id: "call-model".into(),
        deployment_id: "deployment".into(),
        provider_family: "test".into(),
        capability: AiCapability::Language,
        model: "model".into(),
        protocol: "test".into(),
        transport: "memory".into(),
        endpoint_fingerprint: "endpoint".into(),
        config_generation: 1,
        credential_source: None,
        retry_policy: RetryPolicy::default(),
        request_sha256: "a".repeat(64),
    }
}
pub(super) fn evidence() -> RequestEvidence {
    let options = LanguageRequest::new(vec![Message::user_text("fixture").unwrap()]).unwrap();
    RequestEvidence::Available {
        configuration:EvidencePart::inline(json!({"settings":options.settings(),"response_format":options.response_format(),"extensions":options.extensions()}).to_string()),
        system:EvidencePart::inline(serde_json::to_string(&vec![Message::system_text("recorded instructions").unwrap()]).unwrap()),
        tools:EvidencePart::inline(json!({"definitions":options.tools(),"choice":options.tool_choice(),"hosted":options.hosted_tools()}).to_string()),
        manifest:vec![EvidenceContentCount { kind:EvidenceContentKind::Text,count:1,bytes:4 }],
    }
}
pub(super) fn intent(effect: &str, evidence: RequestEvidence) -> SessionFactBody {
    SessionFactBody::ModelIntent {
        turn_id: turn(),
        effect_id: EffectId::new(effect).unwrap(),
        snapshot: snapshot(),
        purpose: ModelPurpose::Conversation,
        price_quote: None,
        evidence,
    }
}
fn event(effect: &str, event: LanguageEvent) -> SessionFactBody {
    SessionFactBody::ModelEvent {
        turn_id: turn(),
        effect_id: EffectId::new(effect).unwrap(),
        event,
        purpose: ModelEventPurpose::Conversation,
    }
}
fn delta(effect: &str, text: &str) -> SessionFactBody {
    event(
        effect,
        LanguageEvent::ContentDelta {
            index: 0,
            delta: ContentDelta::Text(text.into()),
        },
    )
}
fn finished(effect: &str) -> SessionFactBody {
    event(
        effect,
        LanguageEvent::Finished {
            reason: FinishReason::Stop,
            replay: None,
        },
    )
}
pub(super) async fn append(
    store: &MemoryStore,
    header: &SessionHeader,
    after: u64,
    bodies: Vec<SessionFactBody>,
) -> u64 {
    let end = after + bodies.len() as u64;
    append_history_fixture(
        store,
        AppendBatch {
            session_id: header.session_id().clone(),
            expected_seq: after,
            header: if after == 0 && store.header(header.session_id()).await.is_err() {
                Some(header.clone())
            } else {
                None
            },
            facts: bodies
                .into_iter()
                .enumerate()
                .map(|(index, body)| {
                    Arc::new(SessionFact::new(after + index as u64 + 1, 1, body).unwrap())
                })
                .collect(),
        },
    )
    .await
    .unwrap();
    end
}
fn options(include: &str, format: ExportFormat) -> ExportOptions {
    let mut options = ExportOptions {
        format,
        ..Default::default()
    };
    options.select(include).unwrap();
    options
}
async fn collect(store: Arc<MemoryStore>, header: SessionHeader, options: ExportOptions) -> String {
    let source = export(store, header, options, CancellationToken::new())
        .await
        .unwrap();
    let mut bytes = Vec::new();
    write_stream(source, &mut bytes).await.unwrap();
    String::from_utf8(bytes).unwrap()
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn sections_are_exact_redacted_unicode_and_same_in_both_encodings() {
    let store = Arc::new(MemoryStore::new());
    let header = header();
    append(
        &store,
        &header,
        0,
        vec![
            accepted("你好 🦀"),
            intent("one", evidence()),
            event(
                "one",
                LanguageEvent::ContentStarted {
                    index: 1,
                    content: ContentStart::Reasoning,
                },
            ),
            event(
                "one",
                LanguageEvent::ContentDelta {
                    index: 1,
                    delta: ContentDelta::Reasoning("private readable thought".into()),
                },
            ),
            event("one", LanguageEvent::ContentFinished { index: 1 }),
            event(
                "one",
                LanguageEvent::ContentStarted {
                    index: 0,
                    content: ContentStart::Text,
                },
            ),
            delta("one", "visible "),
            delta("one", "answer"),
            event("one", LanguageEvent::ContentFinished { index: 0 }),
            finished("one"),
        ],
    )
    .await;
    store.take_fact_read_cursors();
    let metadata = collect(
        store.clone(),
        header.clone(),
        options("h", ExportFormat::Json),
    )
    .await;
    assert!(!metadata.contains("hidden instructions"));
    assert!(!metadata.contains("recorded instructions"));
    assert!(
        store.take_fact_read_cursors().is_empty(),
        "header-only export must not read Facts"
    );
    for format in [ExportFormat::Json, ExportFormat::Markdown] {
        let text = collect(store.clone(), header.clone(), options("m", format)).await;
        assert!(text.contains("你好 🦀"));
        if format == ExportFormat::Markdown {
            assert!(text.contains("visible answer"));
        } else {
            let value: Value = serde_json::from_str(&text).unwrap();
            let visible: String = value["messages"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|record| record["event"]["delta"]["type"] == "text")
                .filter_map(|record| record["event"]["delta"]["value"].as_str())
                .collect();
            assert_eq!(visible, "visible answer");
        }
        assert!(
            !text.contains("private readable thought") && !text.contains("recorded instructions")
        );
        let text = collect(store.clone(), header.clone(), options("r", format)).await;
        assert!(text.contains("private readable thought"));
        if format == ExportFormat::Json {
            let value: Value = serde_json::from_str(&text).unwrap();
            assert_eq!(value.as_object().unwrap().len(), 1);
            assert!(
                !value["messages"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|r| r.get("replay").is_some())
            );
        }
    }
    let all = collect(
        store.clone(),
        header.clone(),
        options("h,r,pie,lpr,last-provider-response", ExportFormat::Json),
    )
    .await;
    let value: Value = serde_json::from_str(&all).unwrap();
    assert_eq!(value.as_object().unwrap().len(), 5);
    assert_eq!(
        value["last_provider_request"]["availability"], "available",
        "{all}"
    );
    assert_eq!(
        value["last_provider_request"]["request"]["messages"][0]["content"][0]["content"]["text"],
        "recorded instructions"
    );
    assert_eq!(
        value["last_provider_response"]["effect_id"],
        value["last_provider_request"]["effect_id"]
    );
    assert_eq!(value["last_provider_response"]["raw"], false);
    let response = collect(
        store,
        header,
        options("last-provider-response", ExportFormat::Json),
    )
    .await;
    assert!(!response.contains("private readable thought"));
}

#[tokio::test]
async fn cut_is_fixed_before_lazy_consumption_and_partial_generation_does_not_replace_last_pair() {
    let store = Arc::new(MemoryStore::new());
    let header = header();
    let end = append(
        &store,
        &header,
        0,
        vec![
            accepted("original"),
            intent("completed", evidence()),
            finished("completed"),
            intent("partial", evidence()),
            delta("partial", "partial record"),
        ],
    )
    .await;
    let source = export(
        store.clone(),
        header.clone(),
        options("m,lpr,last-provider-response", ExportFormat::Json),
        CancellationToken::new(),
    )
    .await
    .unwrap();
    append(
        &store,
        &header,
        end,
        vec![delta("partial", "AFTER CUT"), finished("partial")],
    )
    .await;
    let mut bytes = Vec::new();
    write_stream(source, &mut bytes).await.unwrap();
    let text = String::from_utf8(bytes).unwrap();
    assert!(!text.contains("AFTER CUT"));
    assert!(text.contains("partial record"));
    let value: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(value["last_provider_request"]["effect_id"], "completed");
    assert_eq!(value["last_provider_response"]["effect_id"], "completed");
    assert!(
        value["messages"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r["partial"] == true)
    );
}

#[tokio::test]
async fn empty_history_and_missing_evidence_have_explicit_availability() {
    let store = Arc::new(MemoryStore::new());
    let header = header();
    store
        .commit_agent(rsi_agent_store_protocol::AtomicAgentCommit {
            sessions: vec![rsi_agent_store_protocol::AtomicSessionAppend {
                session_id: header.session_id().clone(),
                expected_fact_seq: 0,
                expected_control_seq: 0,
                header: Some(header.clone()),
                facts: vec![],
                controls: vec![
                    AgentControlRecord::new(
                        1,
                        1,
                        AgentControlRecordBody::MessageAccepted {
                            message: AgentMessage {
                                message_id: MessageId::new("queued").unwrap(),
                                source: AgentMessageSource::Human,
                                content: vec![AgentMessageContent::Text {
                                    text: "queued only".into(),
                                }],
                                options: MessageOptions::default(),
                            },
                            root_session_id: header.session_id().clone(),
                            delivery: MessageDelivery::NextTurn,
                            bound_turn_id: None,
                            target: MessageTarget::NextTurn,
                            wake_required: true,
                        },
                    )
                    .unwrap(),
                ],
            }],
            required_active_activations: vec![],
            quiescent_descendants_of: None,
        })
        .await
        .unwrap();
    let text = collect(
        store.clone(),
        header.clone(),
        options("m,lpr,last-provider-response", ExportFormat::Json),
    )
    .await;
    let value: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(value["messages"], json!([]));
    assert_eq!(
        value["last_provider_response"]["reason"],
        "no_completed_conversation"
    );
    append(
        &store,
        &header,
        0,
        vec![
            accepted("budget"),
            intent(
                "one",
                RequestEvidence::Unavailable {
                    reason: EvidenceUnavailable::Budget,
                },
            ),
            finished("one"),
        ],
    )
    .await;
    let text = collect(store, header, options("lpr", ExportFormat::Json)).await;
    let value: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(
        value["last_provider_request"]["availability"],
        "unavailable"
    );
    assert_eq!(
        value["last_provider_request"]["evidence"]["reason"],
        "budget"
    );
}

#[tokio::test]
async fn complete_history_over_32_mib_is_read_lazily_in_bounded_pages() {
    let store = Arc::new(MemoryStore::new());
    let header = header();
    let text = "界🦀".repeat(32_768);
    let mut bodies = vec![accepted("large"), intent("one", evidence())];
    bodies.extend((0..160).map(|_| delta("one", &text)));
    bodies.push(finished("one"));
    append(&store, &header, 0, bodies).await;
    store.take_fact_read_cursors();
    let mut source = export(
        store.clone(),
        header.clone(),
        options("m", ExportFormat::Json),
        CancellationToken::new(),
    )
    .await
    .unwrap();
    let mut verifier = ExportVerifier::default();
    verifier
        .accept(&source.next().await.unwrap().unwrap(), None)
        .unwrap();
    assert!(store.take_fact_read_cursors().is_empty());
    let mut chunks = 0;
    let mut reads = Vec::new();
    while let Some(event) = source.next().await {
        let event = event.unwrap();
        verifier.accept(&event, None).unwrap();
        if matches!(event, ExportEvent::Chunk { .. }) {
            chunks += 1;
        }
        let next = store.take_fact_read_cursors();
        assert!(
            next.len() <= 1,
            "one consumer pull cannot aggregate history"
        );
        reads.extend(next);
        if chunks == 8 {
            assert_eq!(
                reads.len(),
                1,
                "producer must stop at the consumer's backpressure"
            );
        }
    }
    assert!(verifier.finish().unwrap() > 32 * 1024 * 1024);
    assert_eq!(reads, vec![0, 32, 64, 96, 128, 160]);
    assert!(chunks > 500);
    append(
        &store,
        &header,
        163,
        vec![intent("next", evidence()), finished("next")],
    )
    .await;
    let diagnostic: Value =
        serde_json::from_str(&collect(store, header, options("lpr", ExportFormat::Json)).await)
            .unwrap();
    assert_eq!(
        diagnostic["last_provider_request"]["availability"],
        "unavailable"
    );
    assert_eq!(
        diagnostic["last_provider_request"]["reason"],
        "semantic_reconstruction_unavailable"
    );
    assert_eq!(
        diagnostic["last_provider_request"]["evidence"]["availability"],
        "available"
    );
}

#[tokio::test]
async fn inherits_only_the_frozen_direct_parent_interval() {
    let store = Arc::new(MemoryStore::new());
    let parent = header();
    let mut spawn = accepted("not inherited");
    if let SessionFactBody::TurnAccepted { turn_id, .. } = &mut spawn {
        *turn_id = TurnId::new("spawn").unwrap();
    }
    append(
        &store,
        &parent,
        0,
        vec![
            accepted("parent inherited"),
            SessionFactBody::TurnTerminal {
                turn_id: turn(),
                outcome: TurnOutcome::Completed,
                result: None,
            },
            spawn,
        ],
    )
    .await;
    let boundary = store
        .resolve_fork_boundary(
            parent.session_id(),
            &TurnId::new("spawn").unwrap(),
            ForkTurnSelection::All,
        )
        .await
        .unwrap();
    let origin = ForkOrigin {
        parent_session_id: parent.session_id().clone(),
        root_session_id: parent.session_id().clone(),
        path: AgentPath::new(vec![1]).unwrap(),
        task_name: "child".into(),
        parent_header_fingerprint: parent.fingerprint().unwrap(),
        invoking_turn_id: TurnId::new("spawn").unwrap(),
        resolved_after_seq: boundary.resolved_after_seq,
        resolved_terminal_seq: boundary.resolved_terminal_seq,
        terminal_prefix_sha256: boundary.terminal_prefix_sha256,
        resolved_terminal_control_seq: boundary.resolved_terminal_control_seq,
        terminal_control_prefix_sha256: boundary.terminal_control_prefix_sha256,
        requested_turns: ForkTurnSelection::All,
        effective_turns: boundary.effective_turns,
    };
    let child = parent
        .forked_child(
            SessionId::new("child").unwrap(),
            2,
            origin,
            ModelSelection::baseline(parent.settings()),
        )
        .unwrap();
    append(&store, &child, 0, vec![accepted("child own")]).await;
    let text = collect(store.clone(), child, options("m", ExportFormat::Json)).await;
    assert!(
        text.contains("parent inherited")
            && text.contains("child own")
            && !text.contains("not inherited")
    );
    let text = collect(store, parent, options("m", ExportFormat::Json)).await;
    assert!(!text.contains("child own"));
}

#[tokio::test]
async fn inherited_last_request_does_not_replay_before_the_selected_interval() {
    let store = Arc::new(MemoryStore::new());
    let parent = header();
    let mut previous = accepted("outside the selection");
    if let SessionFactBody::TurnAccepted { turn_id, .. } = &mut previous {
        *turn_id = TurnId::new("previous").unwrap();
    }
    let mut spawn = accepted("spawn");
    if let SessionFactBody::TurnAccepted { turn_id, .. } = &mut spawn {
        *turn_id = TurnId::new("spawn").unwrap();
    }
    append(
        &store,
        &parent,
        0,
        vec![
            previous,
            SessionFactBody::TurnTerminal {
                turn_id: TurnId::new("previous").unwrap(),
                outcome: TurnOutcome::Completed,
                result: None,
            },
            accepted("selected"),
            intent("selected-call", evidence()),
            finished("selected-call"),
            SessionFactBody::TurnTerminal {
                turn_id: turn(),
                outcome: TurnOutcome::Completed,
                result: None,
            },
            spawn,
        ],
    )
    .await;
    let selection = ForkTurnSelection::Last(1);
    let boundary = store
        .resolve_fork_boundary(
            parent.session_id(),
            &TurnId::new("spawn").unwrap(),
            selection.clone(),
        )
        .await
        .unwrap();
    let origin = ForkOrigin {
        parent_session_id: parent.session_id().clone(),
        root_session_id: parent.session_id().clone(),
        path: AgentPath::new(vec![1]).unwrap(),
        task_name: "child".into(),
        parent_header_fingerprint: parent.fingerprint().unwrap(),
        invoking_turn_id: TurnId::new("spawn").unwrap(),
        resolved_after_seq: boundary.resolved_after_seq,
        resolved_terminal_seq: boundary.resolved_terminal_seq,
        terminal_prefix_sha256: boundary.terminal_prefix_sha256,
        resolved_terminal_control_seq: boundary.resolved_terminal_control_seq,
        terminal_control_prefix_sha256: boundary.terminal_control_prefix_sha256,
        requested_turns: selection,
        effective_turns: boundary.effective_turns,
    };
    let child = parent
        .forked_child(
            SessionId::new("limited-child").unwrap(),
            2,
            origin,
            ModelSelection::baseline(parent.settings()),
        )
        .unwrap();
    append(&store, &child, 0, vec![accepted("child")]).await;
    let text = collect(
        store,
        child,
        options("m,lpr,last-provider-response", ExportFormat::Json),
    )
    .await;
    let value: Value = serde_json::from_str(&text).unwrap();
    assert!(!text.contains("outside the selection"));
    assert_eq!(
        value["last_provider_request"]["availability"],
        "unavailable"
    );
    assert_eq!(
        value["last_provider_request"]["evidence"]["availability"],
        "available"
    );
    assert_eq!(
        value["last_provider_response"]["effect_id"],
        "selected-call"
    );
    assert!(
        value["last_provider_request"]["detail"]
            .as_str()
            .unwrap()
            .contains("selected parent interval")
    );
}

fn framed(text: &str) -> Vec<Result<ExportEvent>> {
    vec![
        Ok(ExportEvent::Start {
            session_id: header().session_id().clone(),
            header_sha256: header().fingerprint().unwrap(),
            through_seq: "1".into(),
            options: ExportOptions::default(),
            filename: "export.md".into(),
        }),
        Ok(ExportEvent::Chunk {
            offset: "0".into(),
            text: text.into(),
        }),
        Ok(ExportEvent::Complete {
            bytes: text.len().to_string(),
            sha256: hex::encode(Sha256::digest(text.as_bytes())),
        }),
    ]
}

#[tokio::test]
async fn tools_media_usage_and_provider_private_replay_are_projected_without_payload_loss() {
    use rsi_tools_protocol::{ToolResult, ToolResultIdentity};
    let store = Arc::new(MemoryStore::new());
    let header = header();
    let identity = ToolResultIdentity::new("owner", "tool-effect", "call", "b".repeat(64)).unwrap();
    let media = rsi_media_protocol::MediaRef {
        id: rsi_media_protocol::MediaId::new("c".repeat(64)).unwrap(),
        mime: "image/png".into(),
        bytes: 123,
        width: 2,
        height: 2,
    };
    let replay = ProviderExtension::new("test", 1, json!({"private":"opaque-secret"})).unwrap();
    append(
        &store,
        &header,
        0,
        vec![
            accepted("tools"),
            intent("one", evidence()),
            event(
                "one",
                LanguageEvent::ContentStarted {
                    index: 0,
                    content: ContentStart::ToolCall {
                        id: "call".into(),
                        name: "lookup".into(),
                        kind: ToolCallKind::Function,
                    },
                },
            ),
            event(
                "one",
                LanguageEvent::ContentDelta {
                    index: 0,
                    delta: ContentDelta::ToolArguments(
                        "{\"replay\":\"ordinary tool data\"}".into(),
                    ),
                },
            ),
            event("one", LanguageEvent::ContentFinished { index: 0 }),
            event(
                "one",
                LanguageEvent::Usage {
                    usage: TokenUsage::new(10, 20, None, None, None).unwrap(),
                },
            ),
            event(
                "one",
                LanguageEvent::Finished {
                    reason: FinishReason::ToolCalls,
                    replay: Some(replay),
                },
            ),
            SessionFactBody::ToolIntent {
                turn_id: turn(),
                effect_id: EffectId::new("tool-effect").unwrap(),
                origin: rsi_agent_session_protocol::ToolOrigin::Model {
                    effect_id: EffectId::new("one").unwrap(),
                },
                program_role: rsi_tools_protocol::ToolProgramRole::Unavailable,
                identity: identity.clone(),
                name: "lookup".into(),
                arguments: json!({"replay":"ordinary tool data"}),
                approval: None,
                parallel_safe: false,
            },
            SessionFactBody::ToolResult {
                turn_id: turn(),
                effect_id: EffectId::new("tool-effect").unwrap(),
                identity,
                result: ToolResult::new(json!({"evidence":"saved result"}), vec![], false).unwrap(),
                conclusion: None,
            },
            SessionFactBody::ImageOutput {
                turn_id: turn(),
                effect_id: EffectId::new("image-effect").unwrap(),
                index: 0,
                media: media.clone(),
            },
        ],
    )
    .await;
    for format in [ExportFormat::Json, ExportFormat::Markdown] {
        let text = collect(
            store.clone(),
            header.clone(),
            options("r,last-provider-response", format),
        )
        .await;
        assert!(
            text.contains("ordinary tool data")
                && text.contains("saved result")
                && text.contains(&media.id.to_string())
        );
        assert!(text.contains("input_tokens") && text.contains("20"));
        assert!(!text.contains("opaque-secret"));
    }
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn compaction_does_not_hide_original_history_or_replace_last_conversation() {
    use rsi_agent_context::{ContextLimits, ContextPage, DefaultContextBuilder, ModelContextState};
    let store = Arc::new(MemoryStore::new());
    let header = header();
    let mut bodies = vec![
        accepted(&"original task ".repeat(5000)),
        intent("conversation", evidence()),
        event(
            "conversation",
            LanguageEvent::ContentStarted {
                index: 0,
                content: ContentStart::Text,
            },
        ),
        delta("conversation", &"original answer ".repeat(7000)),
        event("conversation", LanguageEvent::ContentFinished { index: 0 }),
        finished("conversation"),
        SessionFactBody::TurnTerminal {
            turn_id: turn(),
            outcome: TurnOutcome::Completed,
            result: None,
        },
    ];
    let current = TurnId::new("current").unwrap();
    let mut accepted = accepted("continue");
    if let SessionFactBody::TurnAccepted { turn_id, .. } = &mut accepted {
        *turn_id = current.clone();
    }
    bodies.push(accepted);
    let end = append(&store, &header, 0, bodies).await;
    let facts: Vec<_> = store
        .read_facts(header.session_id(), 0, 32)
        .await
        .unwrap()
        .facts
        .into_iter()
        .map(Arc::new)
        .collect();
    let mut state = ModelContextState::open(
        Arc::new(DefaultContextBuilder::default()),
        header.clone(),
        ContextLimits::default(),
    )
    .unwrap();
    state.ingest(ContextPage::Canonical(&facts)).unwrap();
    let profile = LanguageProfile::new(
        100_000,
        1000,
        10_000,
        ToolDialect::Responses,
        true,
        ImageToolResultCapability::No,
        vec![],
    )
    .unwrap();
    let plan = state
        .plan_compaction(
            &LanguageRequestOptions::default(),
            &ModelRef::new("deployment", "model").unwrap(),
            &profile,
            Some(CompactionTrigger::ProviderContextLimit),
            false,
        )
        .unwrap()
        .unwrap()
        .plan;
    let mut summary = vec![
        intent("summary", evidence()),
        event(
            "summary",
            LanguageEvent::ContentStarted {
                index: 0,
                content: ContentStart::Text,
            },
        ),
        delta("summary", "INTERNAL SUMMARY"),
        event("summary", LanguageEvent::ContentFinished { index: 0 }),
        finished("summary"),
    ];
    for body in &mut summary {
        match body {
            SessionFactBody::ModelIntent {
                turn_id, purpose, ..
            } => {
                *turn_id = current.clone();
                *purpose = ModelPurpose::ContextCompaction(Box::new(plan.clone()));
            }
            SessionFactBody::ModelEvent {
                turn_id, purpose, ..
            } => {
                *turn_id = current.clone();
                *purpose = ModelEventPurpose::ContextCompaction;
            }
            _ => unreachable!(),
        }
    }
    append(&store, &header, end, summary).await;
    let page: Vec<_> = store
        .read_facts(header.session_id(), end, 32)
        .await
        .unwrap()
        .facts
        .into_iter()
        .map(Arc::new)
        .collect();
    state.ingest(ContextPage::Canonical(&page)).unwrap();
    assert!(state.summary_installed(&EffectId::new("summary").unwrap()));
    let text = collect(
        store,
        header,
        options("m,lpr,last-provider-response", ExportFormat::Json),
    )
    .await;
    assert!(text.contains("original task") && text.contains("original answer"));
    assert!(!text.contains("INTERNAL SUMMARY"));
    let value: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(value["last_provider_request"]["effect_id"], "conversation");
    assert_eq!(value["last_provider_response"]["effect_id"], "conversation");
}
#[tokio::test]
async fn native_sink_replaces_atomically_and_preserves_old_file_on_failures_and_cancel() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("nested/output.md");
    write_file(
        Box::pin(futures_util::stream::iter(framed("first"))),
        &path,
        CancellationToken::new(),
    )
    .await
    .unwrap();
    let mut truncated = framed("truncated");
    truncated.pop();
    assert!(
        write_file(
            Box::pin(futures_util::stream::iter(truncated)),
            &path,
            CancellationToken::new()
        )
        .await
        .is_err()
    );
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "first");
    let entered = Arc::new(tokio::sync::Notify::new());
    let ready = entered.clone();
    let source = Box::pin(async_stream::stream! {
        let mut events=framed("cancel");events.pop();
        for event in events { yield event; }
        ready.notify_one(); std::future::pending::<()>().await;
    });
    let destination = path.clone();
    let task =
        tokio::spawn(
            async move { write_file(source, &destination, CancellationToken::new()).await },
        );
    entered.notified().await;
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "first");
    assert_eq!(
        std::fs::read_dir(path.parent().unwrap()).unwrap().count(),
        1
    );
    write_file(
        Box::pin(futures_util::stream::iter(framed("replacement"))),
        &path,
        CancellationToken::new(),
    )
    .await
    .unwrap();
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "replacement");
    // Rename failure also releases the temporary file.
    assert!(
        write_file(
            Box::pin(futures_util::stream::iter(framed("failure"))),
            path.parent().unwrap(),
            CancellationToken::new(),
        )
        .await
        .is_err()
    );
    assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 1);
}

#[tokio::test]
async fn native_cancel_before_commit_preserves_destination() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("output.md");
    std::fs::write(&path, "old").unwrap();
    let stop = CancellationToken::new();
    let token = stop.clone();
    let source = Box::pin(async_stream::stream! {
        for event in framed("new") { yield event; }
        token.cancel();
    });
    assert!(matches!(
        write_file(source, &path, stop).await,
        Err(FileWriteError::Cancelled)
    ));
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "old");
    assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 1);
}

#[tokio::test]
async fn admitted_native_commit_reports_actual_result_after_cancellation_or_waiter_drop() {
    for (drop_waiter, fail) in [(false, false), (false, true), (true, false)] {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("output.md");
        std::fs::write(&path, "old").unwrap();
        let destination = path.clone();
        let stop = CancellationToken::new();
        let token = stop.clone();
        let (entered, enter) = tokio::sync::oneshot::channel();
        let (release, wait) = std::sync::mpsc::channel();
        let (done, finished) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(async move {
            native::write_file_with(
                Box::pin(futures_util::stream::iter(framed("new"))),
                &destination,
                token,
                move |temporary, destination| {
                    entered.send(()).unwrap();
                    wait.recv().unwrap();
                    let result = if fail {
                        Err(encoding("fixture persist failure"))
                    } else {
                        temporary.persist(destination).map_err(encoding)
                    };
                    let _ = done.send(());
                    result
                },
            )
            .await
        });
        enter.await.unwrap();
        stop.cancel();
        assert!(!task.is_finished());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "old");
        if drop_waiter {
            task.abort();
        }
        release.send(()).unwrap();
        finished.await.unwrap();
        if drop_waiter {
            assert!(task.await.unwrap_err().is_cancelled());
        } else if fail {
            assert!(matches!(
                task.await.unwrap(),
                Err(FileWriteError::Export(_))
            ));
        } else {
            assert_eq!(task.await.unwrap().unwrap(), 3);
        }
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            if fail { "old" } else { "new" }
        );
        assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 1);
    }
}

#[test]
fn program_tool_exports_preserve_exact_origin_and_internal_evidence() {
    let fact = SessionFact::new(
        2,
        1,
        SessionFactBody::ToolIntent {
            turn_id: turn(),
            effect_id: EffectId::new("nested").unwrap(),
            origin: ToolOrigin::Program {
                parent_effect_id: EffectId::new("coordinator").unwrap(),
                ordinal: 4,
            },
            program_role: rsi_tools_protocol::ToolProgramRole::Callable,
            identity: rsi_tools_protocol::ToolResultIdentity::new(
                "owner",
                "nested",
                "call",
                "b".repeat(64),
            )
            .unwrap(),
            name: "file_read".into(),
            arguments: json!({"path":"evidence.txt"}),
            approval: None,
            parallel_safe: true,
        },
    )
    .unwrap();
    let mut projection = projection::Projection::default();
    let record = projection
        .record(header().session_id(), &fact, false, None)
        .unwrap()
        .unwrap();
    assert_eq!(
        record["origin"],
        json!({"kind":"program","parent_effect_id":"coordinator","ordinal":4})
    );
    assert_eq!(record["arguments"]["path"], "evidence.txt");
    assert_eq!(record["program_role"], "callable");
    let markdown = projection::Markdown::default().record(&record).unwrap();
    assert!(markdown.contains("coordinator") && markdown.contains("evidence.txt"));
    let restored: SessionFact =
        serde_json::from_value(serde_json::to_value(&fact).unwrap()).unwrap();
    assert_eq!(restored, fact);
}

#[test]
fn workflow_completion_notice_is_visible_in_transcript_and_markdown() {
    let source = rsi_agent_session_protocol::InputMessageSource::Program {
        message_id: rsi_agent_session_protocol::MessageId::new("notice").unwrap(),
        source: rsi_agent_session_protocol::ProgramCompletionSource {
            run_id: rsi_agent_session_protocol::ProgramRunId::new("workflow").unwrap(),
            generation: "a".repeat(64),
            terminal_control_seq: 7,
        },
    };
    let fact = SessionFact::new(
        2,
        1,
        SessionFactBody::InputMessageEntered {
            turn_id: turn(),
            step_id: rsi_agent_session_protocol::StepId::new("step").unwrap(),
            source: source.clone(),
            content: vec![rsi_agent_session_protocol::AgentMessageContent::Text {
                text: "Workflow completed".into(),
            }],
        },
    )
    .unwrap();
    let record = projection::Projection::default()
        .record(header().session_id(), &fact, false, None)
        .unwrap()
        .unwrap();
    assert_eq!(record["source"], serde_json::to_value(source).unwrap());
    let markdown = projection::Markdown::default().record(&record).unwrap();
    assert!(markdown.contains("Workflow completed") && markdown.contains("workflow"));
}
