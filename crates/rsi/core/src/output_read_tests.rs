use super::*;
use rsi_agent_context::{
    ContextBudget, ContextLimits, ContextPage, DefaultContextBuilder, ModelContextState,
};
use rsi_agent_session_protocol::*;
use rsi_ai_protocol::*;
use rsi_sandbox::SandboxMode;
use rsi_spill::{SpillScope, SpillStore as _};

struct History {
    state: ModelContextState,
    facts: Vec<Arc<SessionFact>>,
}
impl History {
    fn new() -> Self {
        let header = SessionHeader::new_local(
            SessionId::new("spill-source").unwrap(),
            1,
            "/workspace",
            AgentPresetId::new("fixture").unwrap(),
            FrozenAgentSettings::new(
                "default",
                "instructions",
                ModelRef::new("fixture", "model").unwrap(),
                SandboxMode::ReadOnly,
                false,
            )
            .unwrap(),
        )
        .unwrap();
        Self {
            state: ModelContextState::open(
                Arc::new(DefaultContextBuilder::default()),
                header,
                ContextLimits::default(),
                ContextBudget::default(),
            )
            .unwrap(),
            facts: vec![],
        }
    }
    fn push(&mut self, body: SessionFactBody) -> Arc<SessionFact> {
        let seq = self.facts.len() as u64 + 1;
        let fact = Arc::new(SessionFact::new(seq, seq, body).unwrap());
        self.state
            .ingest(ContextPage::Canonical(std::slice::from_ref(&fact)))
            .unwrap();
        self.facts.push(fact.clone());
        fact
    }
    fn accept(&mut self, turn: &str, text: &str) {
        let turn = TurnId::new(turn).unwrap();
        let fact = self.push(SessionFactBody::TurnAccepted {
            turn_id: turn.clone(),
            text: text.into(),
            model: None,
            reasoning_effort: None,
            sandbox: SandboxMode::ReadOnly,
            require_approval: false,
        });
        self.push(SessionFactBody::StepInputsPrepared {
            turn_id: turn,
            step_id: StepId::new("input").unwrap(),
            decision: StepInputDecision::enter(vec![PreparedInput::Claimed {
                claim: InputClaimRef::new(fact.seq(), 0).unwrap(),
            }])
            .unwrap(),
        });
    }
    fn model(
        &mut self,
        turn: &str,
        effect: &str,
        purpose: ModelPurpose,
        events: Vec<LanguageEvent>,
    ) {
        let turn = TurnId::new(turn).unwrap();
        let effect = EffectId::new(effect).unwrap();
        let event_purpose = purpose.event_purpose();
        self.push(SessionFactBody::ModelIntent {
            turn_id: turn.clone(),
            effect_id: effect.clone(),
            purpose,
            price_quote: None,
            evidence: RequestEvidence::Unavailable {
                reason: EvidenceUnavailable::NotCaptured,
            },
            snapshot: PreparedCallSnapshot {
                language_settings: None,
                call_id: effect.to_string(),
                deployment_id: "fixture".into(),
                provider_family: "fixture".into(),
                capability: AiCapability::Language,
                model: "model".into(),
                protocol: "fixture".into(),
                transport: "memory".into(),
                endpoint_fingerprint: "fixture".into(),
                config_generation: 1,
                credential_source: None,
                retry_policy: RetryPolicy::default(),
                request_sha256: "a".repeat(64),
            },
        });
        self.push(SessionFactBody::ModelStarted {
            turn_id: turn.clone(),
            effect_id: effect.clone(),
        });
        self.push(SessionFactBody::ModelAttemptSettled {
            turn_id: turn,
            effect_id: effect,
            purpose: event_purpose,
            attempt: ModelAttemptLog::from_events(events.into_iter().map(|e| (0, e)), true)
                .unwrap(),
        });
    }
    fn wire(&self) -> String {
        serde_json::to_string(
            &self
                .state
                .build(LanguageRequestOptions::default(), &profile())
                .unwrap(),
        )
        .unwrap()
    }
}
fn profile() -> LanguageProfile {
    LanguageProfile::new(
        100_000,
        1000,
        10_000,
        ToolDialect::Responses,
        true,
        ImageToolResultCapability::No,
        vec![],
    )
    .unwrap()
}
fn text_events(text: String) -> Vec<LanguageEvent> {
    vec![
        LanguageEvent::ContentStarted {
            index: 0,
            content: ContentStart::Text,
        },
        LanguageEvent::ContentDelta {
            index: 0,
            delta: ContentDelta::Text(text),
        },
        LanguageEvent::ContentFinished { index: 0 },
        LanguageEvent::Finished {
            reason: FinishReason::Stop,
            replay: None,
        },
    ]
}

#[derive(Debug)]
struct NoExecution;
#[async_trait]
impl rsi_sandbox::Sandbox for NoExecution {
    async fn workspace_read(
        &self,
        _: rsi_sandbox::WorkspaceReadRequest,
    ) -> rsi_sandbox::Result<rsi_sandbox::WorkspaceReadScope> {
        panic!("output_read has no filesystem authority");
    }
    async fn confine(
        &self,
        _: rsi_sandbox::ProcessRequest,
    ) -> rsi_sandbox::Result<rsi_sandbox::ConfinedProcess> {
        panic!("output_read cannot execute");
    }
}
fn execution(root: &std::path::Path) -> ToolExecution {
    ToolExecution::from_start(
        "output-read".into(),
        rsi_tools_protocol::ToolStart {
            cancellation: tokio_util::sync::CancellationToken::new(),
            policy: rsi_tools_protocol::ToolExecutionPolicy {
                mode: SandboxMode::ReadOnly,
                cwd: root.into(),
                workspace: root.into(),
            },
            sandbox: Arc::new(NoExecution),
            job_scope: None,
            extensions: rsi_tools_protocol::ToolExecutionExtensions::default(),
        },
    )
    .unwrap()
    .0
}

#[tokio::test]
async fn read_pages_include_hex_only_when_safe_text_changes_raw_bytes() {
    let root = tempfile::tempdir().unwrap();
    let mut config = rsi_spill_local::SpillLocalConfig::new(root.path().join("spill"));
    config.minimum_free_bytes = 0;
    let store = rsi_spill_local::LocalSpillStore::open(config)
        .await
        .unwrap();
    let reference = store
        .save_text(SpillScope::new("session", "page-text"), "HEAD\0中🦀TAIL")
        .await
        .unwrap();
    let reader = OutputReadTool(store.clone());
    for (offset, limit, text, raw_hex) in [
        (0, 4, "HEAD", None),
        (4, 1, "\u{fffd}", Some("00")),
        (5, 1, "\u{fffd}", Some("e4")),
        (5, 3, "中", None),
    ] {
        let page = reader
            .execute(
                json!({"id": reference.locator, "offset": offset, "limit": limit}),
                execution(root.path()),
            )
            .await
            .unwrap();
        assert!(!page.is_error);
        assert_eq!(page.value["text"], text);
        assert_eq!(page.value.get("bytes_hex").and_then(Value::as_str), raw_hex);
    }
    store.shutdown().await;
}

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "One retained object is followed through policy, pruning, compaction, restart and expiry."
)]
async fn spill_survives_pruning_compaction_source_and_restart_then_expires_explicitly() {
    let root = tempfile::tempdir().unwrap();
    let mut config = rsi_spill_local::SpillLocalConfig::new(root.path().join("spill"));
    config.minimum_free_bytes = 0;
    let store = rsi_spill_local::LocalSpillStore::open(config.clone())
        .await
        .unwrap();
    let scope = SpillScope::new("session", "spill-source");
    let original = format!("HEAD{}TAIL", "中🦀".repeat(100_000));
    let reference = store.save_text(scope.clone(), &original).await.unwrap();
    let preview = rsi_spill_policy::retain_text(&original, &reference, 12_500).unwrap();
    let mut result = ToolResult::new(
        json!({"canonical":"unchanged"}),
        vec![ToolContent::Text {
            text: preview.clone(),
        }],
        false,
    )
    .unwrap();
    result.spills.push(reference.clone());

    let mut history = History::new();
    history.accept("old", "Read full output");
    history.model(
        "old",
        "model-tools",
        ModelPurpose::Conversation,
        vec![
            LanguageEvent::ContentStarted {
                index: 0,
                content: ContentStart::ToolCall {
                    id: "lookup".into(),
                    name: "lookup".into(),
                    kind: ToolCallKind::Function,
                },
            },
            LanguageEvent::ContentDelta {
                index: 0,
                delta: ContentDelta::ToolArguments("{}".into()),
            },
            LanguageEvent::ContentFinished { index: 0 },
            LanguageEvent::Finished {
                reason: FinishReason::ToolCalls,
                replay: None,
            },
        ],
    );
    let turn = TurnId::new("old").unwrap();
    let effect = EffectId::new("tool").unwrap();
    let identity = rsi_tools_protocol::ToolResultIdentity::new(
        "fixture",
        "invocation",
        "lookup",
        "a".repeat(64),
    )
    .unwrap();
    history.push(SessionFactBody::ToolIntent {
        turn_id: turn.clone(),
        effect_id: effect.clone(),
        identity: identity.clone(),
        origin: ToolOrigin::Model {
            effect_id: EffectId::new("model-tools").unwrap(),
        },
        program_role: rsi_tools_protocol::ToolProgramRole::Unavailable,
        name: "lookup".into(),
        arguments: json!({}),
        approval: None,
        parallel_safe: false,
    });
    history.push(SessionFactBody::ToolStarted {
        turn_id: turn.clone(),
        effect_id: effect.clone(),
        identity: identity.clone(),
    });
    let source = history.push(SessionFactBody::ToolResult {
        turn_id: turn.clone(),
        effect_id: effect,
        identity,
        result,
        conclusion: None,
    });
    history.push(SessionFactBody::TurnTerminal {
        turn_id: turn,
        outcome: TurnOutcome::Completed,
        result: None,
    });
    // A fork seeds the same immutable Facts and locator; it does not copy cache bytes.
    let parent = history.state.header();
    let child_header = parent
        .forked_child(
            SessionId::new("spill-child").unwrap(),
            2,
            ForkOrigin {
                parent: HistoryOrigin {
                    parent_session_id: parent.session_id().clone(),
                    parent_header_fingerprint: parent.fingerprint().unwrap(),
                    point: ForkPoint::BeforeTurn {
                        turn_id: TurnId::new("current").unwrap(),
                    },
                    resolved_after_seq: 0,
                    resolved_terminal_seq: history.facts.len() as u64,
                    terminal_prefix_sha256: fact_prefix_sha256(
                        history.facts.iter().map(Arc::as_ref),
                    )
                    .unwrap(),
                    resolved_terminal_control_seq: 1,
                    terminal_control_prefix_sha256: "c".repeat(64),
                    requested_turns: ForkTurnSelection::All,
                    effective_turns: 1,
                }
                .into(),
                root_session_id: parent.session_id().clone(),
                path: AgentPath::new(vec![1]).unwrap(),
                task_name: "child".into(),
            },
            ModelSelection::baseline(parent.settings()),
            None,
        )
        .unwrap();
    let mut child = ModelContextState::open(
        Arc::new(DefaultContextBuilder::default()),
        child_header.clone(),
        ContextLimits::default(),
        ContextBudget::default(),
    )
    .unwrap();
    child
        .ingest(ContextPage::ForkSource(&child_header))
        .unwrap();
    child.ingest(ContextPage::ForkSeed(&history.facts)).unwrap();
    child.ingest(ContextPage::FinishSeed).unwrap();
    assert!(
        serde_json::to_string(
            &child
                .build(LanguageRequestOptions::default(), &profile())
                .unwrap()
        )
        .unwrap()
        .contains(reference.locator.as_str())
    );
    history.accept("current", "Continue");
    let pruned = history.wire();
    assert!(pruned.contains("middle pruned"));
    assert!(pruned.contains(reference.locator.as_str()));
    let restored = history
        .state
        .restored(&history.state.checkpoint().unwrap())
        .unwrap();
    assert_eq!(
        serde_json::to_string(
            &restored
                .build(LanguageRequestOptions::default(), &profile())
                .unwrap()
        )
        .unwrap(),
        pruned
    );

    history.model(
        "current",
        "recent",
        ModelPurpose::Conversation,
        text_events("recent ".repeat(15_000)),
    );
    let planned = history
        .state
        .plan_compaction(
            &LanguageRequestOptions::default(),
            &ModelRef::new("fixture", "model").unwrap(),
            &profile(),
            Some(CompactionTrigger::ProviderContextLimit),
            false,
        )
        .unwrap()
        .unwrap();
    assert!(
        serde_json::to_string(planned.request.messages())
            .unwrap()
            .contains(reference.locator.as_str())
    );
    assert!(
        planned
            .plan
            .sources
            .iter()
            .any(|s| s.turn.as_str() == "old")
    );
    history.model(
        "current",
        "summary",
        ModelPurpose::ContextCompaction(Box::new(planned.plan)),
        text_events("Prior evidence summarized; consult original sources.".into()),
    );
    assert!(
        history
            .state
            .summary_installed(&EffectId::new("summary").unwrap())
    );
    // Durable replay is independent of the cache and preserves original source Facts.
    let facts: Vec<Arc<SessionFact>> =
        serde_json::from_slice(&serde_json::to_vec(&history.facts).unwrap()).unwrap();
    let mut replay = History::new();
    replay.state.ingest(ContextPage::Canonical(&facts)).unwrap();
    assert_eq!(replay.wire(), history.wire());
    let retained = &facts[usize::try_from(source.seq()).unwrap() - 1];
    let field = rsi_conversation::select_field(
        retained,
        rsi_conversation::SourceRef {
            seq: source.seq(),
            field: rsi_conversation::FactField::ToolText { index: 0 },
        },
    )
    .unwrap();
    let mut offset = preview.len().saturating_sub(4096);
    while !preview.is_char_boundary(offset) {
        offset += 1;
    }
    let window = field.window(offset, 4096).unwrap();
    assert!(window.text.contains(reference.locator.as_str()));
    let SessionFactBody::ToolResult { result, .. } = retained.body() else {
        panic!("source result");
    };
    assert_eq!(result.spills, std::slice::from_ref(&reference));
    assert_eq!(result.value, json!({"canonical":"unchanged"}));

    store.shutdown().await;
    let store = rsi_spill_local::LocalSpillStore::open(config)
        .await
        .unwrap();
    let reader = OutputReadTool(store.clone());
    let mut bytes = Vec::new();
    while bytes.len() < original.len() {
        let page = reader
            .execute(
                json!({"id":result.spills[0].locator,"offset":bytes.len(),"limit":65536}),
                execution(root.path()),
            )
            .await
            .unwrap();
        assert!(!page.is_error);
        let text = page.value["text"].as_str().unwrap();
        let next = usize::try_from(page.value["next_offset"].as_u64().unwrap()).unwrap();
        let raw = &original.as_bytes()[bytes.len()..next];
        assert_eq!(
            page.value.get("bytes_hex").is_some(),
            text.as_bytes() != raw
        );
        bytes.extend(
            page.value["bytes_hex"]
                .as_str()
                .map_or_else(|| text.as_bytes().to_vec(), |hex| hex::decode(hex).unwrap()),
        );
        assert_eq!(
            page.value["next_offset"].as_u64().unwrap(),
            bytes.len() as u64
        );
    }
    assert_eq!(bytes, original.as_bytes());
    store.cleanup(&scope).await.unwrap();
    let expired = reader
        .execute(json!({"id":reference.locator}), execution(root.path()))
        .await
        .unwrap();
    assert!(expired.is_error);
    assert_eq!(expired.value["code"], "output_unavailable");
    store.shutdown().await;
}
