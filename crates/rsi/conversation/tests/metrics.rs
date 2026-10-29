use rsi_agent_session_protocol::{
    EffectId, ModelEventPurpose, ModelPurpose, SessionFact, SessionFactBody, TurnId,
};
use rsi_ai_protocol::{
    AiCapability, AiError, DispatchStatus, ErrorKind, ErrorPhase, FinishReason, LanguageEvent,
    PreparedCallSnapshot, RetryPolicy, TokenUsage,
};
use rsi_conversation::MetricsReducer;

fn fact(seq: u64, body: SessionFactBody) -> SessionFact {
    SessionFact::new(seq, seq * 10, body).unwrap()
}
fn turn() -> TurnId {
    TurnId::new("turn").unwrap()
}
fn effect(value: &str) -> EffectId {
    EffectId::new(value).unwrap()
}
fn intent(seq: u64, id: &str) -> SessionFact {
    fact(
        seq,
        SessionFactBody::ModelIntent {
            evidence: rsi_agent_session_protocol::RequestEvidence::Unavailable {
                reason: rsi_agent_session_protocol::EvidenceUnavailable::NotCaptured,
            },
            price_quote: None,
            turn_id: turn(),
            effect_id: effect(id),
            purpose: ModelPurpose::Conversation,
            snapshot: PreparedCallSnapshot {
                call_id: id.into(),
                deployment_id: "fixture".into(),
                provider_family: "fixture".into(),
                capability: AiCapability::Language,
                model: "model".into(),
                protocol: "chat".into(),
                transport: "http".into(),
                endpoint_fingerprint: "local".into(),
                config_generation: 1,
                credential_source: None,
                retry_policy: RetryPolicy::default(),
                request_sha256: "0".repeat(64),
                language_settings: None,
            },
        },
    )
}
fn settled(
    seq: u64,
    id: &str,
    usage: Option<TokenUsage>,
    failed: bool,
    elapsed: u64,
) -> SessionFact {
    use rsi_ai_protocol::{ContentDelta, ContentStart};
    let mut events = vec![
        (
            0,
            LanguageEvent::ContentStarted {
                index: 0,
                content: ContentStart::Text,
            },
        ),
        (
            1,
            LanguageEvent::ContentDelta {
                index: 0,
                delta: ContentDelta::Text("answer".into()),
            },
        ),
        (2, LanguageEvent::ContentFinished { index: 0 }),
    ];
    if let Some(usage) = usage {
        events.push((elapsed, LanguageEvent::Usage { usage }));
    }
    let terminal = if failed {
        LanguageEvent::Failed {
            error: AiError::new(
                ErrorKind::Transport,
                ErrorPhase::Stream,
                DispatchStatus::Dispatched,
                "connection lost",
            )
            .unwrap(),
            replay: None,
        }
    } else {
        LanguageEvent::Finished {
            reason: FinishReason::Stop,
            replay: None,
        }
    };
    events.push((elapsed, terminal));
    fact(
        seq,
        SessionFactBody::ModelAttemptSettled {
            turn_id: turn(),
            effect_id: effect(id),
            purpose: ModelEventPurpose::Conversation,
            attempt: rsi_agent_session_protocol::ModelAttemptLog::from_events(events, true)
                .unwrap(),
        },
    )
}

#[test]
fn captured_input_capacity_uses_frozen_caps_and_rejects_invalid_decode() {
    use rsi_agent_session_protocol::{ModelOutputBudget, OutputBudgetTable};
    use rsi_ai_protocol::{
        ImageToolResultCapability, LanguageProfile, ModelRef, PreparedLanguageSettings, ToolDialect,
    };
    let profile = LanguageProfile::new(
        100_000,
        1000,
        10_000,
        ToolDialect::Responses,
        false,
        ImageToolResultCapability::No,
        vec![],
    )
    .unwrap();
    for (cap, expected) in [
        (None, 99_000),
        (Some(500), 99_000),
        (Some(9000), 91_000),
        (Some(10_000), 90_000),
    ] {
        let table = OutputBudgetTable::new(
            cap.into_iter()
                .map(|cap| ModelOutputBudget {
                    model: ModelRef::new("fixture", "model").unwrap(),
                    max_output_tokens: cap,
                })
                .collect(),
        )
        .unwrap();
        let mut reducer = MetricsReducer::new(table);
        let mut body = intent(1, "capacity").into_body();
        let SessionFactBody::ModelIntent { snapshot, .. } = &mut body else {
            unreachable!()
        };
        snapshot.language_settings =
            Some(PreparedLanguageSettings::new(profile.clone(), None).unwrap());
        for fact in [
            fact(1, body),
            settled(
                2,
                "capacity",
                Some(TokenUsage::new(76_000, 0, None, None, None).unwrap()),
                false,
                20,
            ),
        ] {
            reducer.observe(&fact).unwrap();
        }
        let summary = reducer.summary();
        assert_eq!(
            summary.last_context.as_ref().unwrap().input_capacity(),
            expected
        );
        assert_eq!(profile.input_capacity_tokens(cap).unwrap(), expected);
        let wire = serde_json::to_value(summary).unwrap();
        assert_eq!(
            serde_json::from_value::<rsi_conversation::SessionMetrics>(wire.clone()).unwrap(),
            *summary
        );
        for invalid in [0, 10_001, u32::MAX] {
            let mut bad = wire.clone();
            bad["last_context"]["max_output_tokens"] = serde_json::json!(invalid);
            assert!(serde_json::from_value::<rsi_conversation::SessionMetrics>(bad).is_err());
        }
    }
}

#[test]
fn request_metadata_backfill_preserves_actual_default_effort_failed_usage_and_elapsed_time() {
    use rsi_ai_protocol::{
        ImageToolResultCapability, LanguageProfile, PreparedLanguageSettings, ReasoningEffortId,
        ReasoningEffortProfile, ToolDialect,
    };
    let effort = ReasoningEffortId::new("max").unwrap();
    let profile = LanguageProfile::new(
        128_000,
        4096,
        8192,
        ToolDialect::Responses,
        false,
        ImageToolResultCapability::No,
        vec![],
    )
    .unwrap()
    .with_reasoning_efforts(
        ReasoningEffortProfile::new(vec![effort.clone()], Some(effort)).unwrap(),
    );
    let mut body = intent(1, "actual").body().clone();
    if let SessionFactBody::ModelIntent { snapshot, .. } = &mut body {
        snapshot.language_settings = Some(PreparedLanguageSettings::new(profile, None).unwrap());
    }
    let intent = fact(1, body);
    let started = SessionFact::new(
        2,
        1000,
        SessionFactBody::ModelStarted {
            turn_id: turn(),
            effect_id: effect("actual"),
        },
    )
    .unwrap();
    let failure = settled(
        3,
        "actual",
        Some(TokenUsage::new(10, 4, None, None, Some(2)).unwrap()),
        true,
        1250,
    );
    let mut state = rsi_conversation::RequestPresentation::default();
    state.observe(&failure);
    assert!(state.title().contains("model not loaded"));
    assert!(state.title().contains("10 in / 4 out"));
    for fact in [&started, &intent, &intent] {
        state.observe(fact);
    }
    assert_eq!(state.title(), "model · max · 10 in / 4 out · 1.2s · failed");
    assert_eq!(state.intent_seq(), Some(1));
    assert!(state.owned_bytes() < 1024);
}

#[test]
fn tree_aggregation_preserves_unknown_subsets_and_rejects_overflow_atomically() {
    use rsi_conversation::{SessionMetrics, UsageTotals};
    let summary = |input, cache| SessionMetrics {
        through_seq: 1,
        attempts: 1,
        reported_attempts: 1,
        tokens: TokenUsage::new(input, 1, cache, None, None).unwrap(),
        configured_cost: rsi_conversation::ConfiguredCost {
            missing_price: 1,
            ..Default::default()
        },
        ..Default::default()
    };
    let mut totals = UsageTotals::default();
    totals.add_session(&summary(10, Some(2))).unwrap();
    totals.add_session(&SessionMetrics::default()).unwrap();
    assert_eq!(
        totals.tokens.cache_read_tokens(),
        Some(2),
        "empty members cannot erase known subsets"
    );
    totals.add_session(&summary(5, None)).unwrap();
    assert_eq!(totals.tokens.input_tokens(), 15);
    assert_eq!(totals.tokens.cache_read_tokens(), None);
    let before = totals.clone();
    assert!(totals.add_session(&summary(u64::MAX - 1, None)).is_err());
    assert_eq!(totals, before);
}

#[test]
fn failed_usage_and_retry_are_counted_once_and_unknown_breakdowns_stay_unknown() {
    let mut reducer = MetricsReducer::default();
    reducer.observe(&intent(1, "first")).unwrap();
    reducer
        .observe(&fact(
            2,
            SessionFactBody::ModelStarted {
                turn_id: turn(),
                effect_id: effect("first"),
            },
        ))
        .unwrap();
    let failure = settled(
        3,
        "first",
        Some(TokenUsage::new(10, 4, Some(3), Some(2), Some(1)).unwrap()),
        true,
        20,
    );
    reducer.observe(&failure).unwrap();
    reducer.observe(&failure).unwrap();
    assert_eq!(reducer.summary().failed_attempts, 1);
    assert_eq!(
        reducer.summary().last_attempt.as_ref().unwrap().elapsed_ms,
        Some(20)
    );
    assert!(reducer.summary().last_context.is_none());
    assert_eq!(reducer.summary().timing.model_ms, 20);
    assert_eq!(reducer.summary().timing.decode_ms, 19);
    assert_eq!(reducer.summary().timing.decode_output_tokens, 4);
    reducer.observe(&intent(4, "retry")).unwrap();
    reducer
        .observe(&settled(
            5,
            "retry",
            Some(TokenUsage::new(7, 2, None, None, None).unwrap()),
            false,
            30,
        ))
        .unwrap();
    let summary = reducer.summary();
    summary.validate().unwrap();
    assert_eq!(summary.attempts, 2);
    assert_eq!(summary.reported_attempts, 2);
    assert_eq!(summary.tokens.input_tokens(), 17);
    assert_eq!(summary.tokens.output_tokens(), 6);
    assert_eq!(summary.tokens.cache_read_tokens(), None);
    assert_eq!(summary.last_attempt.as_ref().unwrap().elapsed_ms, None);
    assert_eq!(summary.through_seq, 5);
}

#[test]
fn sequence_and_counter_failures_do_not_advance_the_cursor() {
    let mut reducer = MetricsReducer::default();
    assert!(reducer.observe(&intent(2, "gap")).is_err());
    assert_eq!(reducer.summary().through_seq, 0);
    reducer.observe(&intent(1, "first")).unwrap();
    assert!(
        reducer
            .observe(&settled(2, "wrong", Some(TokenUsage::default()), false, 2))
            .is_err()
    );
    assert_eq!(reducer.summary().through_seq, 1);
    let first = settled(
        2,
        "first",
        Some(TokenUsage::new(u64::MAX, 0, None, None, None).unwrap()),
        false,
        2,
    );
    reducer.observe(&first).unwrap();
    reducer.observe(&first).unwrap();
    assert_eq!(reducer.summary().reported_attempts, 1);
    reducer.observe(&intent(3, "second")).unwrap();
    let before = serde_json::to_value(&reducer).unwrap();
    assert!(
        reducer
            .observe(&settled(
                4,
                "second",
                Some(TokenUsage::new(1, 0, None, None, None).unwrap()),
                false,
                2
            ))
            .is_err()
    );
    assert_eq!(serde_json::to_value(&reducer).unwrap(), before);
    assert_eq!(reducer.summary().tokens.input_tokens(), u64::MAX);
}

#[test]
fn configured_prices_include_failed_usage_and_preserve_partial_currency_totals() {
    let mut reducer = MetricsReducer::default();
    for (index, currency, cache) in [(0, "USD", Some(5)), (1, "CNY", Some(0)), (2, "USD", None)] {
        let seq = index * 2 + 1;
        let id = format!("priced-{index}");
        let mut body = intent(seq, &id).into_body();
        let SessionFactBody::ModelIntent { price_quote, .. } = &mut body else {
            unreachable!()
        };
        *price_quote = Some(serde_json::from_value(serde_json::json!({"model":{"deployment":"fixture","model":"model"}, "endpoint_fingerprint":"local", "currency":currency, "input_nanos":100, "output_nanos":200,"cache_read_nanos":10})).unwrap());
        reducer.observe(&fact(seq, body)).unwrap();
        let failure = settled(
            seq + 1,
            &id,
            Some(TokenUsage::new(10, 2, cache, None, None).unwrap()),
            true,
            20,
        );
        reducer.observe(&failure).unwrap();
        reducer.observe(&failure).unwrap();
    }
    reducer.observe(&intent(7, "missing-price")).unwrap();
    reducer.summary().validate().unwrap();
    let cost = &reducer.summary().configured_cost;
    assert!(!cost.is_complete());
    assert_eq!(cost.priced_attempts, 2);
    assert_eq!(cost.missing_breakdown, 1);
    assert_eq!(cost.missing_price, 1);
    assert_eq!(
        cost.totals,
        vec![
            rsi_conversation::CurrencyCost {
                currency: "CNY".into(),
                nanos: 1400
            },
            rsi_conversation::CurrencyCost {
                currency: "USD".into(),
                nanos: 950
            }
        ]
    );
    assert_eq!(reducer.summary().failed_attempts, 3);
}

#[test]
fn wrong_settlement_is_atomic_and_fragment_count_does_not_change_attempt_count() {
    use rsi_agent_session_protocol::ModelAttemptBuilder;
    use rsi_ai_protocol::{ContentDelta, ContentStart};
    let mut reducer = MetricsReducer::default();
    reducer.observe(&intent(1, "streaming")).unwrap();
    let mut builder = ModelAttemptBuilder::new();
    builder
        .push(
            0,
            &LanguageEvent::ContentStarted {
                index: 0,
                content: ContentStart::Text,
            },
        )
        .unwrap();
    for elapsed in 1..=10000 {
        builder
            .push(
                elapsed,
                &LanguageEvent::ContentDelta {
                    index: 0,
                    delta: ContentDelta::Text("界".into()),
                },
            )
            .unwrap();
    }
    builder
        .push(
            10001,
            &LanguageEvent::Finished {
                reason: FinishReason::MaxTokens,
                replay: None,
            },
        )
        .unwrap();
    let attempt = builder.finish(true).unwrap();
    let body = |id| SessionFactBody::ModelAttemptSettled {
        turn_id: turn(),
        effect_id: effect(id),
        purpose: ModelEventPurpose::Conversation,
        attempt: attempt.clone(),
    };
    let before = serde_json::to_value(&reducer).unwrap();
    assert!(reducer.observe(&fact(2, body("foreign"))).is_err());
    assert_eq!(serde_json::to_value(&reducer).unwrap(), before);
    reducer.observe(&fact(2, body("streaming"))).unwrap();
    assert_eq!(reducer.summary().attempts, 1);
    assert_eq!(reducer.summary().through_seq, 2);
}

#[test]
fn timing_uses_recorded_boundaries_and_decode_evidence_without_double_counting() {
    let mut reducer = MetricsReducer::default();
    let accepted: SessionFactBody = serde_json::from_value(serde_json::json!({
        "type":"turn_accepted", "turn_id":"turn", "text":"hello", "model":null,
        "reasoning_effort":null, "sandbox":"workspace-write", "require_approval":false
    }))
    .unwrap();
    reducer
        .observe(&SessionFact::new(1, 100, accepted).unwrap())
        .unwrap();
    reducer.observe(&intent(2, "timed")).unwrap();
    reducer
        .observe(
            &SessionFact::new(
                3,
                200,
                SessionFactBody::ModelStarted {
                    turn_id: turn(),
                    effect_id: effect("timed"),
                },
            )
            .unwrap(),
        )
        .unwrap();
    let completed = settled(
        4,
        "timed",
        Some(TokenUsage::new(100, 20, Some(80), None, None).unwrap()),
        false,
        101,
    );
    reducer.observe(&completed).unwrap();
    reducer.observe(&completed).unwrap();
    let terminal = SessionFact::new(
        5,
        500,
        SessionFactBody::TurnTerminal {
            turn_id: turn(),
            outcome: rsi_agent_session_protocol::TurnOutcome::Completed,
            result: None,
        },
    )
    .unwrap();
    reducer.observe(&terminal).unwrap();
    reducer.observe(&terminal).unwrap();
    let timing = reducer.summary().timing.clone();
    assert_eq!(timing.turns, 1);
    assert_eq!(timing.timed_turns, 1);
    assert_eq!(timing.total_ms, 400);
    assert_eq!(timing.timed_attempts, 1);
    assert_eq!(timing.model_ms, 101);
    assert_eq!(timing.first_token_attempts, 1);
    assert_eq!(timing.first_token_ms, 1);
    assert_eq!(timing.decode_ms, 100);
    assert_eq!(timing.decode_output_tokens, 20);
    reducer.summary().validate().unwrap();
    reducer.observe(&intent(6, "missing-start")).unwrap();
    reducer
        .observe(&settled(
            7,
            "missing-start",
            Some(TokenUsage::new(10, 2, None, None, None).unwrap()),
            false,
            100,
        ))
        .unwrap();
    assert_eq!(reducer.summary().timing, timing);
}

#[test]
fn queued_turns_retain_independent_wall_time_boundaries() {
    let mut reducer = MetricsReducer::default();
    for (seq, timestamp, id) in [(1, 100, "first"), (2, 150, "queued")] {
        let body = serde_json::from_value(serde_json::json!({
            "type":"turn_accepted", "turn_id":id, "text":"hello", "model":null,
            "reasoning_effort":null, "sandbox":"workspace-write", "require_approval":false
        }))
        .unwrap();
        reducer
            .observe(&SessionFact::new(seq, timestamp, body).unwrap())
            .unwrap();
    }
    for (seq, timestamp, id) in [(3, 300, "first"), (4, 500, "queued")] {
        let terminal = SessionFact::new(
            seq,
            timestamp,
            SessionFactBody::TurnTerminal {
                turn_id: TurnId::new(id).unwrap(),
                outcome: rsi_agent_session_protocol::TurnOutcome::Completed,
                result: None,
            },
        )
        .unwrap();
        reducer.observe(&terminal).unwrap();
        reducer.observe(&terminal).unwrap();
    }
    assert_eq!(reducer.summary().timing.turns, 2);
    assert_eq!(reducer.summary().timing.timed_turns, 2);
    assert_eq!(reducer.summary().timing.total_ms, 550);
}

#[test]
fn turn_timing_is_bounded_and_terminal_settlement_releases_capacity() {
    let mut reducer = MetricsReducer::default();
    let limit = rsi_agent_session_protocol::MAXIMUM_LIVE_TURNS as u64;
    let accepted = |seq, id| {
        SessionFact::new(seq, 100, serde_json::from_value(serde_json::json!({
        "type":"turn_accepted", "turn_id":format!("turn-{id}"), "text":"hello", "model":null,
        "reasoning_effort":null, "sandbox":"workspace-write", "require_approval":false
    })).unwrap()).unwrap()
    };
    for id in 0..limit {
        reducer.observe(&accepted(id + 1, id)).unwrap();
    }
    let before = serde_json::to_value(&reducer).unwrap();
    assert!(reducer.observe(&accepted(limit + 1, limit)).is_err());
    assert_eq!(serde_json::to_value(&reducer).unwrap(), before);
    reducer
        .observe(
            &SessionFact::new(
                limit + 1,
                200,
                SessionFactBody::TurnTerminal {
                    turn_id: TurnId::new("turn-0").unwrap(),
                    outcome: rsi_agent_session_protocol::TurnOutcome::Completed,
                    result: None,
                },
            )
            .unwrap(),
        )
        .unwrap();
    reducer.observe(&accepted(limit + 2, limit)).unwrap();
    assert_eq!(reducer.summary().timing.turns, limit + 1);
    assert_eq!(reducer.summary().timing.total_ms, 100);
}

#[test]
fn timing_validation_rejects_claims_without_corresponding_evidence() {
    let mut summary = rsi_conversation::SessionMetrics::default();
    summary.timing.model_ms = 1;
    assert!(summary.validate().is_err());
    summary.timing.model_ms = 0;
    summary.timing.first_token_attempts = 1;
    assert!(summary.validate().is_err());
}

#[test]
fn timing_overflow_is_atomic() {
    let mut reducer = MetricsReducer::default();
    reducer.observe(&intent(1, "large")).unwrap();
    reducer
        .observe(&fact(
            2,
            SessionFactBody::ModelStarted {
                turn_id: turn(),
                effect_id: effect("large"),
            },
        ))
        .unwrap();
    reducer
        .observe(&settled(3, "large", None, false, u64::MAX))
        .unwrap();
    reducer.observe(&intent(4, "overflow")).unwrap();
    reducer
        .observe(&fact(
            5,
            SessionFactBody::ModelStarted {
                turn_id: turn(),
                effect_id: effect("overflow"),
            },
        ))
        .unwrap();
    let before = serde_json::to_value(&reducer).unwrap();
    assert!(
        reducer
            .observe(&settled(6, "overflow", None, false, 100))
            .is_err()
    );
    assert_eq!(serde_json::to_value(&reducer).unwrap(), before);
}
