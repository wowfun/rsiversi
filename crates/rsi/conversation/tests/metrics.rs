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
fn event(seq: u64, id: &str, event: LanguageEvent) -> SessionFact {
    fact(
        seq,
        SessionFactBody::ModelEvent {
            turn_id: turn(),
            effect_id: effect(id),
            purpose: ModelEventPurpose::Conversation,
            event,
        },
    )
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
    let usage = event(
        3,
        "actual",
        LanguageEvent::Usage {
            usage: TokenUsage::new(10, 4, None, None, Some(2)).unwrap(),
        },
    );
    let failure = SessionFact::new(
        4,
        2250,
        SessionFactBody::ModelEvent {
            turn_id: turn(),
            effect_id: effect("actual"),
            purpose: ModelEventPurpose::Conversation,
            event: LanguageEvent::Failed {
                error: AiError::new(
                    ErrorKind::Transport,
                    ErrorPhase::Stream,
                    DispatchStatus::Dispatched,
                    "lost",
                )
                .unwrap(),
                replay: None,
            },
        },
    )
    .unwrap();
    let mut state = rsi_conversation::RequestPresentation::default();
    state.observe(&failure);
    assert!(state.title().contains("model not loaded"));
    assert!(state.title().contains("usage unknown"));
    for fact in [&usage, &started, &intent, &intent] {
        state.observe(fact);
    }
    assert_eq!(state.title(), "model · max · 10 in / 4 out · 1.2s · failed");
    assert_eq!(state.intent_seq(), Some(1));
    assert!(state.owned_bytes() < 1024);
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
    let usage = event(
        3,
        "first",
        LanguageEvent::Usage {
            usage: TokenUsage::new(10, 4, Some(3), Some(2), Some(1)).unwrap(),
        },
    );
    reducer.observe(&usage).unwrap();
    reducer.observe(&usage).unwrap();
    reducer
        .observe(&event(
            4,
            "first",
            LanguageEvent::Failed {
                error: AiError::new(
                    ErrorKind::Transport,
                    ErrorPhase::Stream,
                    DispatchStatus::Dispatched,
                    "connection lost",
                )
                .unwrap(),
                replay: None,
            },
        ))
        .unwrap();
    assert_eq!(reducer.summary().failed_attempts, 1);
    assert_eq!(
        reducer.summary().last_attempt.as_ref().unwrap().elapsed_ms,
        Some(20)
    );
    assert!(reducer.summary().last_context.is_none());
    reducer.observe(&intent(5, "retry")).unwrap();
    reducer
        .observe(&event(
            6,
            "retry",
            LanguageEvent::Usage {
                usage: TokenUsage::new(7, 2, None, None, None).unwrap(),
            },
        ))
        .unwrap();
    reducer
        .observe(&event(
            7,
            "retry",
            LanguageEvent::Finished {
                reason: FinishReason::Stop,
                replay: None,
            },
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
    assert_eq!(summary.through_seq, 7);
}

#[test]
fn sequence_and_counter_failures_do_not_advance_the_cursor() {
    let mut reducer = MetricsReducer::default();
    assert!(reducer.observe(&intent(2, "gap")).is_err());
    assert_eq!(reducer.summary().through_seq, 0);
    reducer.observe(&intent(1, "first")).unwrap();
    assert!(
        reducer
            .observe(&event(
                2,
                "wrong",
                LanguageEvent::Usage {
                    usage: TokenUsage::default()
                }
            ))
            .is_err()
    );
    assert_eq!(reducer.summary().through_seq, 1);
    reducer
        .observe(&event(
            2,
            "first",
            LanguageEvent::Usage {
                usage: TokenUsage::new(u64::MAX, 0, None, None, None).unwrap(),
            },
        ))
        .unwrap();
    assert!(
        reducer
            .observe(&event(
                3,
                "first",
                LanguageEvent::Usage {
                    usage: TokenUsage::default()
                }
            ))
            .is_err()
    );
    assert_eq!(reducer.summary().reported_attempts, 1);
    reducer
        .observe(&event(
            3,
            "first",
            LanguageEvent::Finished {
                reason: FinishReason::Stop,
                replay: None,
            },
        ))
        .unwrap();
    reducer.observe(&intent(4, "second")).unwrap();
    assert!(
        reducer
            .observe(&event(
                5,
                "second",
                LanguageEvent::Usage {
                    usage: TokenUsage::new(1, 0, None, None, None).unwrap()
                }
            ))
            .is_err()
    );
    assert_eq!(reducer.summary().through_seq, 4);
    assert_eq!(reducer.summary().tokens.input_tokens(), u64::MAX);
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
fn configured_prices_include_failed_usage_and_preserve_partial_currency_totals() {
    let mut reducer = MetricsReducer::default();
    for (index, currency, cache) in [(0, "USD", Some(5)), (1, "CNY", Some(0)), (2, "USD", None)] {
        let seq = index * 3 + 1;
        let mut body = intent(seq, "priced").into_body();
        let SessionFactBody::ModelIntent { price_quote, .. } = &mut body else {
            unreachable!()
        };
        *price_quote = Some(serde_json::from_value(serde_json::json!({"model":{"deployment":"fixture","model":"model"}, "endpoint_fingerprint":"local", "currency":currency, "input_nanos":100, "output_nanos":200,"cache_read_nanos":10})).unwrap());
        reducer.observe(&fact(seq, body)).unwrap();
        let usage = event(
            seq + 1,
            "priced",
            LanguageEvent::Usage {
                usage: TokenUsage::new(10, 2, cache, None, None).unwrap(),
            },
        );
        reducer.observe(&usage).unwrap();
        reducer.observe(&usage).unwrap();
        reducer
            .observe(&event(
                seq + 2,
                "priced",
                LanguageEvent::Failed {
                    error: AiError::new(
                        ErrorKind::Transport,
                        ErrorPhase::Stream,
                        DispatchStatus::Dispatched,
                        "connection lost",
                    )
                    .unwrap(),
                    replay: None,
                },
            ))
            .unwrap();
    }
    reducer.observe(&intent(10, "missing-price")).unwrap();
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
                nanos: 1_400
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
fn content_events_preserve_summary_storage_and_reject_wrong_request_atomically() {
    let mut reducer = MetricsReducer::default();
    reducer.observe(&intent(1, "first")).unwrap();
    reducer
        .observe(&event(
            2,
            "first",
            LanguageEvent::Finished {
                reason: FinishReason::Stop,
                replay: None,
            },
        ))
        .unwrap();
    reducer.observe(&intent(3, "streaming")).unwrap();
    let model = reducer
        .summary()
        .last_attempt
        .as_ref()
        .unwrap()
        .model
        .model()
        .as_ptr();
    let delta = |seq, id| {
        event(
            seq,
            id,
            LanguageEvent::ContentDelta {
                index: 0,
                delta: rsi_ai_protocol::ContentDelta::Text("token".into()),
            },
        )
    };
    let before = serde_json::to_value(&reducer).unwrap();
    assert!(reducer.observe(&delta(4, "foreign")).is_err());
    assert_eq!(serde_json::to_value(&reducer).unwrap(), before);
    for seq in 4..104 {
        reducer.observe(&delta(seq, "streaming")).unwrap();
        assert_eq!(
            reducer
                .summary()
                .last_attempt
                .as_ref()
                .unwrap()
                .model
                .model()
                .as_ptr(),
            model
        );
    }
    assert_eq!(reducer.summary().through_seq, 103);
}
