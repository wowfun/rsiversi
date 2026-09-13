use super::*;
use rsi_agent_context::{ContextPage, DefaultContextBuilder, ModelContextState};
use rsi_agent_session_protocol::{CompactionTrigger, ContextCompactionPlan, ModelPurpose};
use rsi_ai_protocol::{
    ImageToolResultCapability, LanguageProfile, TokenUsage, ToolChoice, ToolDialect,
};
use std::sync::Arc;

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
fn cursor() -> ModelContextState {
    ModelContextState::open(
        Arc::new(DefaultContextBuilder::default()),
        header("instructions"),
        ContextLimits::default(),
    )
    .unwrap()
}

#[test]
fn pressure_without_selectable_history_is_optional_but_forced_pressure_is_a_limit() {
    let mut state = cursor();
    let mut history = Vec::new();
    let model = ModelRef::new("deployment", "model").unwrap();
    let mut bodies = vec![accepted("old", "short task")];
    bodies.extend(model_bodies(
        "old",
        "ordinary",
        ModelPurpose::Conversation,
        "short answer",
        Some(80_000),
        FinishReason::Stop,
    ));
    bodies.extend([
        SessionFactBody::TurnTerminal {
            turn_id: TurnId::new("old").unwrap(),
            outcome: TurnOutcome::Completed,
        },
        accepted("current", "next short task"),
    ]);
    append(&mut state, &mut history, bodies);
    assert!(state.build(vec![]).is_ok());
    for state in [
        &state,
        &state.restored(&state.checkpoint().unwrap()).unwrap(),
    ] {
        assert!(
            state
                .plan_compaction(&model, &profile(), None, false)
                .unwrap()
                .is_none()
        );
        assert_eq!(
            state
                .plan_compaction(
                    &model,
                    &profile(),
                    Some(CompactionTrigger::ProviderContextLimit),
                    false
                )
                .unwrap_err(),
            ContextError::TooLarge
        );
    }
}

#[test]
fn quoted_summary_requests_fit_the_protocol_and_make_progress_through_large_history() {
    for (text, turns) in [("x".repeat(180_000), 160), ("\n\t\"\\".repeat(30_000), 112)] {
        let mut state = cursor();
        let mut history = Vec::new();
        for index in 0..turns {
            let turn = format!("old-{index}");
            append(
                &mut state,
                &mut history,
                vec![
                    accepted(&turn, &text),
                    SessionFactBody::TurnTerminal {
                        turn_id: TurnId::new(turn).unwrap(),
                        outcome: TurnOutcome::Completed,
                    },
                ],
            );
        }
        append(
            &mut state,
            &mut history,
            vec![accepted("current", "continue")],
        );
        state = state.restored(&state.checkpoint().unwrap()).unwrap();
        let mut count = 0;
        while let Some(planned) = state
            .plan_compaction(
                &ModelRef::new("deployment", "model").unwrap(),
                &profile(),
                None,
                false,
            )
            .unwrap()
        {
            assert!(
                planned.request.canonical_bytes().unwrap().len()
                    <= rsi_ai_protocol::MAX_REQUEST_BYTES
            );
            assert!(planned.request.tools().is_empty());
            assert_eq!(planned.request.tool_choice(), &ToolChoice::None);
            let effect = format!("summary-{count}");
            append(
                &mut state,
                &mut history,
                model_bodies(
                    "current",
                    &effect,
                    ModelPurpose::ContextCompaction(Box::new(planned.plan)),
                    "Earlier work retained.",
                    None,
                    FinishReason::Stop,
                ),
            );
            assert!(state.summary_installed(&EffectId::new(effect).unwrap()));
            count += 1;
            assert!(count < 8, "bounded requests must make finite progress");
        }
        assert!(count >= 2);
        assert!(state.build(vec![]).is_ok());
        let mut replay = cursor();
        for page in history.chunks(128) {
            replay.ingest(ContextPage::Canonical(page)).unwrap();
        }
        assert_eq!(wire(&replay), wire(&state));
    }
}

#[test]
fn a_child_summary_consumes_parent_usage_until_new_conversation_usage_arrives() {
    let (_, mut parent, _) = history(Some(80_000));
    parent.pop(); // Only balanced completed parent Turns are inherited.
    let child_header = fork_header("instructions", parent.last().unwrap().seq(), 1);
    let open = || {
        ModelContextState::open(
            Arc::new(DefaultContextBuilder::default()),
            child_header.clone(),
            ContextLimits::default(),
        )
        .unwrap()
    };
    let mut child = open();
    child.ingest(ContextPage::ForkSeed(&parent)).unwrap();
    child.ingest(ContextPage::FinishSeed).unwrap();
    let mut own = Vec::new();
    append(
        &mut child,
        &mut own,
        vec![accepted("child-current", "continue parent work")],
    );
    let model = ModelRef::new("deployment", "model").unwrap();
    let planned = child
        .plan_compaction(&model, &profile(), None, false)
        .unwrap()
        .unwrap();
    assert!(
        matches!(&planned.plan.trigger, CompactionTrigger::Usage { session, .. } if session == header("instructions").session_id())
    );
    append(
        &mut child,
        &mut own,
        model_bodies(
            "child-current",
            "child-summary",
            ModelPurpose::ContextCompaction(Box::new(planned.plan)),
            "Parent task and evidence retained.",
            None,
            FinishReason::Stop,
        ),
    );
    assert!(child.summary_installed(&EffectId::new("child-summary").unwrap()));
    // Keep enough selectable content for stale parent Usage to create another
    // plan, rather than relying on the empty-selection fallback.
    for (effect, text) in [("no-usage", "new evidence "), ("no-usage-tail", "recent ")] {
        append(
            &mut child,
            &mut own,
            model_bodies(
                "child-current",
                effect,
                ModelPurpose::Conversation,
                &text.repeat(12_000),
                None,
                FinishReason::Stop,
            ),
        );
    }
    assert!(
        child
            .plan_compaction(&model, &profile(), None, false)
            .unwrap()
            .is_none()
    );
    assert!(
        child
            .restored(&child.checkpoint().unwrap())
            .unwrap()
            .plan_compaction(&model, &profile(), None, false)
            .unwrap()
            .is_none()
    );
    let mut replay = open();
    replay.ingest(ContextPage::ForkSeed(&parent)).unwrap();
    replay.ingest(ContextPage::FinishSeed).unwrap();
    replay.ingest(ContextPage::Canonical(&own)).unwrap();
    assert!(
        replay
            .plan_compaction(&model, &profile(), None, false)
            .unwrap()
            .is_none()
    );
    assert_eq!(wire(&child), wire(&replay));
    append(
        &mut child,
        &mut own,
        model_bodies(
            "child-current",
            "fresh-usage",
            ModelPurpose::Conversation,
            "latest answer",
            Some(80_000),
            FinishReason::Stop,
        ),
    );
    assert!(
        matches!(child.plan_compaction(&model, &profile(), None, false).unwrap().unwrap().plan.trigger,
        CompactionTrigger::Usage { session, .. } if session == *child_header.session_id())
    );
}

#[test]
fn replay_rejects_a_model_event_with_a_purpose_differing_from_its_intent() {
    let mut bodies = vec![accepted("current", "task")];
    bodies.extend(model_bodies(
        "current",
        "effect",
        ModelPurpose::Conversation,
        "answer",
        None,
        FinishReason::Stop,
    ));
    for body in &mut bodies {
        if let SessionFactBody::ModelEvent { purpose, .. } = body {
            *purpose = rsi_agent_session_protocol::ModelEventPurpose::ContextCompaction;
        }
    }
    let facts = facts_after(0, bodies)
        .into_iter()
        .map(Arc::new)
        .collect::<Vec<_>>();
    let result = cursor().ingest(ContextPage::Canonical(&facts));
    assert!(matches!(result, Err(ContextError::Invalid(_))));
}

fn partial_tool_batch(result_index: Option<u32>) -> Vec<SessionFactBody> {
    let turn = TurnId::new("interrupted").unwrap();
    let effect = EffectId::new("tool-model").unwrap();
    let mut bodies = vec![
        accepted("interrupted", "Keep incomplete tools"),
        SessionFactBody::ModelIntent {
            turn_id: turn.clone(),
            effect_id: effect.clone(),
            snapshot: snapshot(),
            purpose: ModelPurpose::Conversation,
        },
        SessionFactBody::ModelStarted {
            turn_id: turn.clone(),
            effect_id: effect.clone(),
        },
    ];
    for index in 0..2 {
        for event in [
            LanguageEvent::ContentStarted {
                index,
                content: ContentStart::ToolCall {
                    id: format!("missing-{index}"),
                    name: "lookup".into(),
                    kind: ToolCallKind::Function,
                },
            },
            LanguageEvent::ContentDelta {
                index,
                delta: ContentDelta::ToolArguments("{}".into()),
            },
            LanguageEvent::ContentFinished { index },
        ] {
            bodies.push(SessionFactBody::ModelEvent {
                turn_id: turn.clone(),
                effect_id: effect.clone(),
                purpose: rsi_agent_session_protocol::ModelEventPurpose::Conversation,
                event,
            });
        }
    }
    bodies.push(SessionFactBody::ModelEvent {
        turn_id: turn.clone(),
        effect_id: effect.clone(),
        purpose: rsi_agent_session_protocol::ModelEventPurpose::Conversation,
        event: LanguageEvent::Finished {
            reason: FinishReason::ToolCalls,
            replay: None,
        },
    });
    if let Some(index) = result_index {
        bodies.push(SessionFactBody::ToolResult {
            turn_id: turn.clone(),
            effect_id: EffectId::new("tool-result").unwrap(),
            identity: ToolResultIdentity::new(
                "owner",
                "invocation",
                format!("missing-{index}"),
                "b".repeat(64),
            )
            .unwrap(),
            result: ToolResult::new(json!({"result": "partial result"}), vec![], false).unwrap(),
        });
    }
    bodies
}

#[test]
fn interrupted_tool_batches_are_protected_through_compaction_replay_and_fork() {
    for result_index in [None, Some(0), Some(1)] {
        let mut state = cursor();
        let mut history = Vec::new();
        let mut bodies = partial_tool_batch(result_index);
        let mut live = cursor();
        append(&mut live, &mut Vec::new(), bodies.clone());
        assert!(
            matches!(live.plan_compaction(&ModelRef::new("deployment", "model").unwrap(), &profile(), Some(CompactionTrigger::ProviderContextLimit), false), Err(rsi_agent_context::ContextError::Invalid(message)) if message.contains("Tool batch"))
        );
        bodies.push(SessionFactBody::TurnTerminal {
            turn_id: TurnId::new("interrupted").unwrap(),
            outcome: TurnOutcome::Interrupted {
                effect: None,
                reason: "fixture stopped".into(),
            },
        });
        bodies.push(accepted("current", "Original task"));
        append(&mut state, &mut history, bodies);
        append(
            &mut state,
            &mut history,
            model_bodies(
                "current",
                "pressure",
                ModelPurpose::Conversation,
                &"complete evidence ".repeat(8000),
                Some(90_000),
                FinishReason::Stop,
            ),
        );
        append(
            &mut state,
            &mut history,
            model_bodies(
                "current",
                "recent",
                ModelPurpose::Conversation,
                "recent tail",
                Some(90_000),
                FinishReason::Stop,
            ),
        );
        assert!(wire(&state).contains("missing-0"));
        let planned = state
            .plan_compaction(
                &ModelRef::new("deployment", "model").unwrap(),
                &profile(),
                None,
                false,
            )
            .unwrap()
            .unwrap();
        assert!(
            !serde_json::to_string(planned.request.messages())
                .unwrap()
                .contains("missing-")
        );
        append(
            &mut state,
            &mut history,
            model_bodies(
                "current",
                "summary",
                ModelPurpose::ContextCompaction(Box::new(planned.plan)),
                "Verified complete history",
                None,
                FinishReason::Stop,
            ),
        );
        assert!(state.summary_installed(&EffectId::new("summary").unwrap()));
        append(
            &mut state,
            &mut history,
            vec![SessionFactBody::TurnTerminal {
                turn_id: TurnId::new("current").unwrap(),
                outcome: TurnOutcome::Completed,
            }],
        );
        let expected = wire(&state);
        assert!(expected.contains("missing-0") && expected.contains("missing-1"));
        assert_eq!(expected.contains("partial result"), result_index.is_some());
        let mut replay = cursor();
        replay.ingest(ContextPage::Canonical(&history)).unwrap();
        assert_eq!(wire(&replay), expected);
        let mut fork = ModelContextState::open(
            Arc::new(DefaultContextBuilder::default()),
            fork_header("instructions", history.last().unwrap().seq(), 2),
            ContextLimits::default(),
        )
        .unwrap();
        fork.ingest(ContextPage::ForkSeed(&history)).unwrap();
        fork.ingest(ContextPage::FinishSeed).unwrap();
        assert_eq!(wire(&fork), expected);
    }
}

#[test]
fn missed_source_pressure_opportunity_does_not_poison_later_replay() {
    let mut state = cursor();
    let mut history = Vec::new();
    // No model attempt at the trigger: stopped or describe-failed Turns are legal.
    for index in 0..1300 {
        append(
            &mut state,
            &mut history,
            vec![
                accepted(&format!("old-{index}"), "old evidence"),
                SessionFactBody::TurnTerminal {
                    turn_id: TurnId::new(format!("old-{index}")).unwrap(),
                    outcome: TurnOutcome::Interrupted {
                        effect: None,
                        reason: "fixture stopped".into(),
                    },
                },
            ],
        );
    }
    append(
        &mut state,
        &mut history,
        vec![accepted("current", "original task")],
    );
    // Reopening at the missed opportunity must preserve metadata above the trigger.
    state = state.restored(&state.checkpoint().unwrap()).unwrap();
    for index in 0..2 {
        let planned = state
            .plan_compaction(
                &ModelRef::new("deployment", "model").unwrap(),
                &profile(),
                None,
                false,
            )
            .unwrap()
            .unwrap();
        assert_eq!(planned.plan.trigger, CompactionTrigger::CanonicalLimit);
        assert!(planned.plan.sources.len() <= 1024);
        if index == 0 {
            assert_eq!(planned.plan.sources.len(), 1024);
        }
        let effect = format!("summary-{index}");
        append(
            &mut state,
            &mut history,
            model_bodies(
                "current",
                &effect,
                ModelPurpose::ContextCompaction(Box::new(planned.plan)),
                "Old evidence checked",
                None,
                FinishReason::Stop,
            ),
        );
        assert!(state.summary_installed(&EffectId::new(effect).unwrap()));
    }
    let mut replay = cursor();
    replay.ingest(ContextPage::Canonical(&history)).unwrap();
    assert_eq!(wire(&replay), wire(&state));
}

#[test]
fn encoded_plan_bound_keeps_long_identifier_history_recoverable() {
    for maximum_ids in [false, true] {
        let session = if maximum_ids {
            "s".repeat(256)
        } else {
            format!("session-{}", "a".repeat(32))
        };
        let header = SessionHeader::new(
            SessionId::new(session).unwrap(),
            1,
            "/workspace",
            AgentPresetId::new("test-agent").unwrap(),
            header("instructions").settings().clone(),
        )
        .unwrap();
        let open = || {
            ModelContextState::open(
                Arc::new(DefaultContextBuilder::default()),
                header.clone(),
                ContextLimits::default(),
            )
            .unwrap()
        };
        let mut state = open();
        let mut history = Vec::new();
        for index in 0..1100 {
            let turn = if maximum_ids {
                format!("{index:0>256}")
            } else {
                format!("turn-message-{}", 100_000 + index * 10)
            };
            let mut bodies = vec![accepted(&turn, "older task")];
            bodies.extend(model_bodies(
                &turn,
                &format!("model-{index}"),
                ModelPurpose::Conversation,
                "checked older task",
                None,
                FinishReason::Stop,
            ));
            bodies.push(SessionFactBody::TurnTerminal {
                turn_id: TurnId::new(turn).unwrap(),
                outcome: TurnOutcome::Completed,
            });
            append(&mut state, &mut history, bodies);
        }
        append(
            &mut state,
            &mut history,
            vec![accepted("current", "current task")],
        );
        state = state.restored(&state.checkpoint().unwrap()).unwrap();
        let mut installed = 0;
        while let Some(planned) = state
            .plan_compaction(
                &ModelRef::new("deployment", "model").unwrap(),
                &profile(),
                None,
                false,
            )
            .unwrap()
        {
            assert!(serde_json::to_vec(&planned.plan).unwrap().len() <= 256 * 1024);
            assert!(!planned.plan.selections.is_empty());
            let effect = format!("summary-{installed}");
            append(
                &mut state,
                &mut history,
                model_bodies(
                    "current",
                    &effect,
                    ModelPurpose::ContextCompaction(Box::new(planned.plan)),
                    "Older tasks checked.",
                    None,
                    FinishReason::Stop,
                ),
            );
            assert!(state.summary_installed(&EffectId::new(effect).unwrap()));
            installed += 1;
            assert!(installed < 16, "each bounded plan must make progress");
        }
        assert!(installed >= 2);
        let mut replay = open();
        replay.ingest(ContextPage::Canonical(&history)).unwrap();
        assert_eq!(wire(&replay), wire(&state));
        assert!(wire(&state).contains("current task"));
    }
}
fn append(
    state: &mut ModelContextState,
    history: &mut Vec<Arc<SessionFact>>,
    bodies: Vec<SessionFactBody>,
) {
    let page: Vec<_> = facts_after(history.last().map_or(0, |fact| fact.seq()), bodies)
        .into_iter()
        .map(Arc::new)
        .collect();
    state.ingest(ContextPage::Canonical(&page)).unwrap();
    history.extend(page);
}
fn accepted(id: &str, text: &str) -> SessionFactBody {
    SessionFactBody::TurnAccepted {
        turn_id: TurnId::new(id).unwrap(),
        text: text.into(),
        model: None,
        sandbox: SandboxMode::WorkspaceWrite,
        require_approval: false,
    }
}
fn model_bodies(
    turn: &str,
    effect: &str,
    purpose: ModelPurpose,
    text: &str,
    usage: Option<u64>,
    finish: FinishReason,
) -> Vec<SessionFactBody> {
    let event_purpose = purpose.event_purpose();
    let turn = TurnId::new(turn).unwrap();
    let effect = EffectId::new(effect).unwrap();
    let mut bodies = vec![
        SessionFactBody::ModelIntent {
            turn_id: turn.clone(),
            effect_id: effect.clone(),
            snapshot: snapshot(),
            purpose,
        },
        SessionFactBody::ModelStarted {
            turn_id: turn.clone(),
            effect_id: effect.clone(),
        },
    ];
    let mut events = vec![
        LanguageEvent::ContentStarted {
            index: 0,
            content: ContentStart::Text,
        },
        LanguageEvent::ContentDelta {
            index: 0,
            delta: ContentDelta::Text(text.into()),
        },
        LanguageEvent::ContentFinished { index: 0 },
    ];
    if let Some(input_tokens) = usage {
        events.push(LanguageEvent::Usage {
            usage: TokenUsage {
                input_tokens,
                ..Default::default()
            },
        });
    }
    events.push(LanguageEvent::Finished {
        reason: finish,
        replay: None,
    });
    bodies.extend(events.into_iter().map(|event| SessionFactBody::ModelEvent {
        purpose: event_purpose,
        turn_id: turn.clone(),
        effect_id: effect.clone(),
        event,
    }));
    bodies
}
fn history(usage: Option<u64>) -> (ModelContextState, Vec<Arc<SessionFact>>, u64) {
    let mut state = cursor();
    let mut history = Vec::new();
    append(
        &mut state,
        &mut history,
        vec![accepted("old", &"older private task ".repeat(5000))],
    );
    append(
        &mut state,
        &mut history,
        model_bodies(
            "old",
            "ordinary",
            ModelPurpose::Conversation,
            &"evidence ".repeat(10_000),
            usage,
            FinishReason::Stop,
        ),
    );
    append(
        &mut state,
        &mut history,
        vec![SessionFactBody::TurnTerminal {
            turn_id: TurnId::new("old").unwrap(),
            outcome: TurnOutcome::Completed,
        }],
    );
    let old_end = history.last().unwrap().seq();
    append(
        &mut state,
        &mut history,
        vec![accepted("current", "original task")],
    );
    (state, history, old_end)
}
fn plan(state: &ModelContextState) -> ContextCompactionPlan {
    let planned = state
        .plan_compaction(
            &ModelRef::new("deployment", "model").unwrap(),
            &profile(),
            Some(CompactionTrigger::ProviderContextLimit),
            false,
        )
        .unwrap()
        .unwrap();
    assert!(planned.request.tools().is_empty());
    assert_eq!(planned.request.tool_choice(), &ToolChoice::None);
    assert_eq!(planned.request.settings().max_output_tokens(), Some(8192));
    planned.plan
}
fn wire(state: &ModelContextState) -> String {
    serde_json::to_string(&state.build(vec![]).unwrap()).unwrap()
}

#[test]
fn pressure_uses_only_matching_successful_usage_and_never_guesses_first_call() {
    let model = ModelRef::new("deployment", "model").unwrap();
    for (usage, pressured) in [(None, false), (Some(79_199), false), (Some(79_200), true)] {
        let (state, _, _) = history(usage);
        assert_eq!(
            state
                .plan_compaction(&model, &profile(), None, false)
                .unwrap()
                .is_some(),
            pressured
        );
        assert!(
            state
                .plan_compaction(
                    &ModelRef::new("different", "model").unwrap(),
                    &profile(),
                    None,
                    false
                )
                .unwrap()
                .is_none()
        );
    }
}

#[test]
fn only_finished_installs_and_summary_usage_does_not_retrigger_pressure() {
    let (mut state, mut history, _) = history(Some(80_000));
    let original = wire(&state);
    let frozen = plan(&state);
    let mut bodies = model_bodies(
        "current",
        "summary",
        ModelPurpose::ContextCompaction(Box::new(frozen)),
        "Task and evidence retained; continue original task.",
        Some(99_999),
        FinishReason::Stop,
    );
    let finished = bodies.pop().unwrap();
    append(&mut state, &mut history, bodies);
    assert!(!state.summary_installed(&EffectId::new("summary").unwrap()));
    assert_eq!(wire(&state), original);
    assert!(state.checkpoint().is_err());
    append(&mut state, &mut history, vec![finished]);
    assert!(state.summary_installed(&EffectId::new("summary").unwrap()));
    let compacted = wire(&state);
    assert!(compacted.len() < original.len());
    assert!(compacted.contains("original task"));
    assert!(compacted.contains("instructions"));
    assert!(!compacted.contains("older private task"));
    assert!(
        state
            .plan_compaction(
                &ModelRef::new("deployment", "model").unwrap(),
                &profile(),
                None,
                false
            )
            .unwrap()
            .is_none()
    );
    let checkpoint = state.checkpoint().unwrap();
    let mut restored = cursor();
    restored.restore(&checkpoint).unwrap();
    assert_eq!(wire(&restored), compacted);
    let mut replayed = cursor();
    replayed.ingest(ContextPage::Canonical(&history)).unwrap();
    assert_eq!(wire(&replayed), compacted);
}

#[test]
fn invalid_stale_and_unsupported_summaries_are_inert_without_becoming_assistant_text() {
    for mode in [
        "max_tokens",
        "empty",
        "oversized",
        "builder",
        "view",
        "unsupported",
        "source",
        "foreign_prior",
    ] {
        let (mut state, mut history, _) = history(None);
        let original = wire(&state);
        let mut frozen = plan(&state);
        let mut text = "INTERNAL SECRET SUMMARY".to_owned();
        let mut reason = FinishReason::Stop;
        match mode {
            "max_tokens" => reason = FinishReason::MaxTokens,
            "empty" => text = " ".into(),
            "oversized" => text = "X".repeat(32 * 1024 + 1),
            "builder" => frozen.builder.semantic_version = "99.0.0".into(),
            "view" => frozen.view_sha256 = "a".repeat(64),
            "unsupported" => frozen.version = 2,
            "source" => frozen.sources[0].facts_sha256 = "a".repeat(64),
            "foreign_prior" => {
                frozen.prior = Some(rsi_agent_session_protocol::CompactionPrior {
                    session: SessionId::new("unselected-parent").unwrap(),
                    effect: EffectId::new("future-summary").unwrap(),
                    finished_seq: u64::MAX,
                    text_sha256: "a".repeat(64),
                });
            }
            _ => unreachable!(),
        }
        append(
            &mut state,
            &mut history,
            model_bodies(
                "current",
                "summary",
                ModelPurpose::ContextCompaction(Box::new(frozen)),
                &text,
                None,
                reason,
            ),
        );
        assert!(
            !state.summary_installed(&EffectId::new("summary").unwrap()),
            "{mode}"
        );
        assert_eq!(wire(&state), original, "{mode}");
    }
}

#[test]
fn fork_reuses_only_summaries_whose_complete_transitive_sources_are_visible() {
    let (mut parent, mut history, old_end) = history(Some(80_000));
    let frozen = plan(&parent);
    append(
        &mut parent,
        &mut history,
        model_bodies(
            "current",
            "summary",
            ModelPurpose::ContextCompaction(Box::new(frozen)),
            "private summary of all earlier work",
            None,
            FinishReason::Stop,
        ),
    );
    append(
        &mut parent,
        &mut history,
        vec![SessionFactBody::TurnTerminal {
            turn_id: TurnId::new("current").unwrap(),
            outcome: TurnOutcome::Completed,
        }],
    );
    for (after_seq, reused) in [(0, true), (old_end, false)] {
        let parent_header = header("instructions");
        let mut origin = fork_header("instructions", history.last().unwrap().seq(), 2)
            .fork_origin()
            .unwrap()
            .clone();
        origin.resolved_after_seq = after_seq;
        origin.effective_turns = if reused { 2 } else { 1 };
        origin.requested_turns = if reused {
            ForkTurnSelection::All
        } else {
            ForkTurnSelection::Last(1)
        };
        let child_header = parent_header
            .forked_child(SessionId::new("fork").unwrap(), 3, origin)
            .unwrap();
        let mut child = ModelContextState::open(
            Arc::new(DefaultContextBuilder::default()),
            child_header,
            ContextLimits::default(),
        )
        .unwrap();
        let selected: Vec<_> = history
            .iter()
            .filter(|fact| fact.seq() > after_seq)
            .cloned()
            .collect();
        child.ingest(ContextPage::ForkSeed(&selected)).unwrap();
        child.ingest(ContextPage::FinishSeed).unwrap();
        assert_eq!(
            wire(&child).contains("private summary of all earlier work"),
            reused
        );
        assert!(wire(&child).contains("original task"));
        if !reused {
            assert!(!wire(&child).contains("older private task"));
        }
    }
}

#[test]
fn long_lived_small_turns_compact_without_lifetime_metadata_growth() {
    long_lived_turns(false);
}

#[test]
fn replacement_instructions_do_not_pin_every_historical_turn() {
    long_lived_turns(true);
}

fn long_lived_turns(replace_instructions: bool) {
    let mut state = cursor();
    let mut history = Vec::new();
    let model = ModelRef::new("deployment", "model").unwrap();
    let mut summaries = 0;
    for index in 0..1300 {
        let turn = format!("small-{index}");
        append(
            &mut state,
            &mut history,
            vec![accepted(&turn, "small task")],
        );
        if replace_instructions {
            for source in [
                InputMessageSource::AgentInstructions {
                    source: "workspace-baseline".into(),
                    sha256: "a".repeat(64),
                    replacement: true,
                    tombstone: false,
                },
                InputMessageSource::SkillCatalog {
                    sha256: "b".repeat(64),
                },
            ] {
                append(
                    &mut state,
                    &mut history,
                    vec![SessionFactBody::InputMessageEntered {
                        turn_id: TurnId::new(&turn).unwrap(),
                        step_id: StepId::new(format!("step-{index}")).unwrap(),
                        source,
                        content: vec![AgentMessageContent::Text {
                            text: format!("Active instruction version {index}."),
                        }],
                    }],
                );
            }
        }
        if let Some(planned) = state
            .plan_compaction(&model, &profile(), None, false)
            .unwrap()
        {
            let effect = format!("summary-{index}");
            append(
                &mut state,
                &mut history,
                model_bodies(
                    &turn,
                    &effect,
                    ModelPurpose::ContextCompaction(Box::new(planned.plan)),
                    "Prior small tasks completed.",
                    None,
                    FinishReason::Stop,
                ),
            );
            assert!(state.summary_installed(&EffectId::new(effect).unwrap()));
            summaries += 1;
        }
        state.build(vec![]).unwrap();
        if replace_instructions {
            assert!(wire(&state).contains(&format!("Active instruction version {index}.")));
        }
        append(
            &mut state,
            &mut history,
            vec![SessionFactBody::TurnTerminal {
                turn_id: TurnId::new(turn).unwrap(),
                outcome: TurnOutcome::Completed,
            }],
        );
    }
    assert!(summaries > 3);
    let checkpoint = state.checkpoint().unwrap();
    assert!(checkpoint.len() < 160 * 1024);
    assert_eq!(wire(&state.restored(&checkpoint).unwrap()), wire(&state));
    let mut replay = cursor();
    for page in history.chunks(128) {
        replay.ingest(ContextPage::Canonical(page)).unwrap();
    }
    assert_eq!(wire(&state), wire(&replay));
}

#[test]
fn instruction_replacement_preserves_other_sources_and_additive_instructions() {
    check_instruction_supersession(false);
}

#[test]
fn tombstone_without_replacement_supersedes_its_own_source() {
    check_instruction_supersession(true);
}

fn check_instruction_supersession(tombstone: bool) {
    let mut state = cursor();
    let mut history = Vec::new();
    append(
        &mut state,
        &mut history,
        vec![accepted("old", &"old task ".repeat(10_000))],
    );
    let instruction =
        |turn: &str, source: &str, replacement, text: &str| SessionFactBody::InputMessageEntered {
            turn_id: TurnId::new(turn).unwrap(),
            step_id: StepId::new("step").unwrap(),
            source: InputMessageSource::AgentInstructions {
                source: source.into(),
                sha256: "a".repeat(64),
                replacement,
                tombstone: tombstone && turn == "current",
            },
            content: vec![AgentMessageContent::Text { text: text.into() }],
        };
    append(
        &mut state,
        &mut history,
        vec![
            instruction("old", "workspace", true, "SUPERSEDED_WORKSPACE"),
            instruction("old", "other-source", true, "KEEP_OTHER_SOURCE"),
            instruction("old", "other-source", false, "KEEP_ADDITIVE_SOURCE"),
            SessionFactBody::TurnTerminal {
                turn_id: TurnId::new("old").unwrap(),
                outcome: TurnOutcome::Completed,
            },
            accepted("current", "current task"),
            instruction("current", "workspace", !tombstone, "CURRENT_WORKSPACE"),
        ],
    );
    // Put the historical instructions outside the retained recent tail.
    append(
        &mut state,
        &mut history,
        model_bodies(
            "current",
            "recent",
            ModelPurpose::Conversation,
            &"recent evidence ".repeat(6000),
            None,
            FinishReason::Stop,
        ),
    );
    let frozen = plan(&state);
    append(
        &mut state,
        &mut history,
        model_bodies(
            "current",
            "summary",
            ModelPurpose::ContextCompaction(Box::new(frozen)),
            "Prior task summarized.",
            None,
            FinishReason::Stop,
        ),
    );
    assert!(state.summary_installed(&EffectId::new("summary").unwrap()));
    let request = wire(&state);
    assert!(!request.contains("SUPERSEDED_WORKSPACE"));
    for expected in [
        "KEEP_OTHER_SOURCE",
        "KEEP_ADDITIVE_SOURCE",
        "CURRENT_WORKSPACE",
    ] {
        assert!(request.contains(expected));
    }
}

#[test]
fn summary_retry_halves_bytes_and_can_skip_an_oversized_whole_unit() {
    let (mut state, mut history, _) = history(None);
    for (index, size) in [1_000_000, 40_000, 40_000, 40_000].into_iter().enumerate() {
        append(
            &mut state,
            &mut history,
            model_bodies(
                "current",
                &format!("unit-{index}"),
                ModelPurpose::Conversation,
                &"x".repeat(size),
                None,
                FinishReason::Stop,
            ),
        );
    }
    let model = ModelRef::new("deployment", "model").unwrap();
    let first = state
        .plan_compaction(
            &model,
            &profile(),
            Some(CompactionTrigger::ProviderContextLimit),
            false,
        )
        .unwrap()
        .unwrap();
    let retry = state
        .plan_compaction(
            &model,
            &profile(),
            Some(CompactionTrigger::ProviderContextLimit),
            true,
        )
        .unwrap()
        .unwrap();
    let first_bytes = serde_json::to_vec(first.request.messages()).unwrap().len();
    let retry_bytes = serde_json::to_vec(retry.request.messages()).unwrap().len();
    assert!(
        retry_bytes < first_bytes / 2,
        "first={first_bytes}, retry={retry_bytes}"
    );
    assert!(
        !serde_json::to_string(retry.request.messages())
            .unwrap()
            .contains(&"x".repeat(1_000_000))
    );
}

#[test]
fn fork_checks_prior_chain_after_transitive_bindings_are_released() {
    let (mut parent, mut history, old_end) = history(None);
    for index in 0..3 {
        append(
            &mut parent,
            &mut history,
            model_bodies(
                "current",
                &format!("work-{index}"),
                ModelPurpose::Conversation,
                &"new evidence ".repeat(7000),
                None,
                FinishReason::Stop,
            ),
        );
        let frozen = plan(&parent);
        assert_eq!(frozen.prior.is_some(), index > 0);
        append(
            &mut parent,
            &mut history,
            model_bodies(
                "current",
                &format!("chain-{index}"),
                ModelPurpose::ContextCompaction(Box::new(frozen)),
                "TRANSITIVE_PRIVATE_SUMMARY",
                None,
                FinishReason::Stop,
            ),
        );
        assert!(parent.summary_installed(&EffectId::new(format!("chain-{index}")).unwrap()));
    }
    append(
        &mut parent,
        &mut history,
        vec![SessionFactBody::TurnTerminal {
            turn_id: TurnId::new("current").unwrap(),
            outcome: TurnOutcome::Completed,
        }],
    );
    for (after, reuse) in [(0, true), (old_end, false)] {
        let mut origin = fork_header("instructions", history.last().unwrap().seq(), 2)
            .fork_origin()
            .unwrap()
            .clone();
        origin.resolved_after_seq = after;
        origin.effective_turns = if reuse { 2 } else { 1 };
        origin.requested_turns = if reuse {
            ForkTurnSelection::All
        } else {
            ForkTurnSelection::Last(1)
        };
        let child_header = header("instructions")
            .forked_child(SessionId::new("chain-fork").unwrap(), 3, origin)
            .unwrap();
        let mut child = ModelContextState::open(
            Arc::new(DefaultContextBuilder::default()),
            child_header,
            ContextLimits::default(),
        )
        .unwrap();
        let selected: Vec<_> = history
            .iter()
            .filter(|fact| fact.seq() > after)
            .cloned()
            .collect();
        child.ingest(ContextPage::ForkSeed(&selected)).unwrap();
        child.ingest(ContextPage::FinishSeed).unwrap();
        assert_eq!(wire(&child).contains("TRANSITIVE_PRIVATE_SUMMARY"), reuse);
    }
}
