use rsi_agent_goal::{
    GoalAction, GoalPhase, GoalReport, GoalReportKind, GoalState, RoundOutcome, RoundSettlement,
};
use rsi_agent_session_protocol::{DomainRequestId, TurnId};

fn id(value: &str) -> DomainRequestId {
    DomainRequestId::new(value).unwrap()
}
fn create(max_rounds: u64, draft: bool) -> GoalState {
    let mut state = GoalState::default();
    state
        .apply(
            GoalAction::Create {
                id: id("task"),
                objective: "Implement and verify the requested behavior".into(),
                constraints: "Preserve unrelated changes".into(),
                max_rounds: Some(max_rounds),
            },
            256,
            draft,
        )
        .unwrap();
    state
}
fn reserved(rounds: u64) -> GoalState {
    let mut state = create(rounds, true);
    state.goal.as_mut().unwrap().reserve().unwrap();
    state
}
fn terminal(outcome: RoundOutcome) -> RoundSettlement {
    RoundSettlement::Turn {
        turn_id: TurnId::new("source").unwrap(),
        outcome,
    }
}

#[test]
fn explicit_policy_block_survives_in_flight_work_and_clears_on_resume_or_completion() {
    let block = |code: &str, message: &str| GoalAction::Block {
        id: id("task"),
        code: code.into(),
        message: message.into(),
    };
    let mut state = reserved(2);
    for (code, message) in [
        ("A", "stop"),
        ("bad-", "stop"),
        ("a--1", "stop"),
        ("2a", "stop"),
        ("a_b", "stop"),
        ("a", " "),
        ("a", " padded "),
    ] {
        let before = state.clone();
        assert!(state.apply(block(code, message), 2, false).is_err());
        assert_eq!(state, before);
    }
    state
        .apply(block("needs-2fa", "Choose an account."), 2, false)
        .unwrap();
    let before = state.clone();
    assert!(
        state
            .apply(block("new", "Different message"), 2, false)
            .is_err()
    );
    assert_eq!(state, before);
    report(&mut state, GoalReportKind::Complete);
    let goal = state.goal.as_mut().unwrap();
    let message = goal.reservation.as_ref().unwrap().message_id.clone();
    goal.settle(&message, terminal(RoundOutcome::Completed))
        .unwrap();
    assert_eq!(goal.phase, GoalPhase::Blocked);
    assert_eq!(goal.block_code.as_deref(), Some("needs-2fa"));
    assert_eq!(goal.reason.as_deref(), Some("Choose an account."));
    assert!(goal.verified_report().is_some());
    let raw = serde_json::to_vec(&state).unwrap();
    for action in [
        GoalAction::Resume { id: id("task") },
        GoalAction::Complete {
            id: id("task"),
            source: rsi_agent_goal::GoalCompleter::Owner,
        },
    ] {
        let mut cold: GoalState = serde_json::from_slice(&raw).unwrap();
        cold.validate().unwrap();
        cold.apply(action, 2, false).unwrap();
        let goal = cold.goal.unwrap();
        assert!(goal.reason.is_none() && goal.block_code.is_none());
        assert_eq!(goal.allocated_rounds, 1);
    }
}

#[test]
fn policy_codes_use_remaining_projection_space_and_leave_room_for_in_flight_evidence() {
    let mut state = create(2, false);
    let goal = state.goal.as_mut().unwrap();
    goal.objective = "\\".repeat(8192);
    goal.constraints = "\"".repeat(4096);
    goal.reserve().unwrap();
    let block = |size| GoalAction::Block {
        id: id("task"),
        code: "a".repeat(size),
        message: "\"".repeat(4096),
    };
    let before = state.clone();
    assert!(state.apply(block(32 * 1024), 2, false).is_err());
    assert_eq!(state, before);
    state.apply(block(20 * 1024), 2, false).unwrap();
    let goal = state.goal.as_mut().unwrap();
    let message = goal.reservation.as_ref().unwrap().message_id.clone();
    let turn = TurnId::new("t".repeat(rsi_agent_session_protocol::MAXIMUM_AGENT_IDENTIFIER_BYTES))
        .unwrap();
    goal.record_report(GoalReport {
        source_turn: turn.clone(),
        kind: GoalReportKind::Complete,
        evidence: "\\".repeat(4096),
    })
    .unwrap();
    goal.settle(
        &message,
        RoundSettlement::Turn {
            turn_id: turn,
            outcome: RoundOutcome::PartialFailed,
        },
    )
    .unwrap();
    assert!(rsi_agent_session_protocol::ProjectionValue::encode(&state).is_ok());
    assert_eq!(state.goal.unwrap().block_code.unwrap().len(), 20 * 1024);
}

#[test]
fn plugin_completion_stops_rounds_without_verifying_or_replacing_the_model_report() {
    let mut untouched = create(2, false);
    let before = untouched.clone();
    assert!(
        untouched
            .apply(
                GoalAction::Complete {
                    id: id("task"),
                    source: rsi_agent_goal::GoalCompleter::Plugin {
                        name: String::new()
                    },
                },
                2,
                false
            )
            .is_err()
    );
    assert_eq!(untouched, before);
    let plugin = rsi_agent_goal::GoalCompleter::Plugin {
        name: "@fixture/goal".into(),
    };
    for disposition in [
        RoundOutcome::Failed,
        RoundOutcome::Cancelled,
        RoundOutcome::Completed,
    ] {
        let mut state = reserved(2);
        report(&mut state, GoalReportKind::Blocked);
        state
            .apply(
                GoalAction::Complete {
                    id: id("task"),
                    source: plugin.clone(),
                },
                2,
                false,
            )
            .unwrap();
        let goal = state.goal.as_mut().unwrap();
        assert!(goal.verified_report().is_none());
        assert!(goal.reserve().is_err());
        let message = goal.reservation.as_ref().unwrap().message_id.clone();
        goal.settle(&message, terminal(disposition)).unwrap();
        assert_eq!(goal.phase, GoalPhase::Completed);
        assert_eq!(goal.completed_by.as_ref(), Some(&plugin));
        assert_eq!(
            goal.verified_report().is_some(),
            disposition == RoundOutcome::Completed
        );
        assert_eq!(goal.report.as_ref().unwrap().kind, GoalReportKind::Blocked);
        assert_eq!(goal.allocated_rounds, 1);
        state.validate().unwrap();
    }
}

#[test]
fn a_paused_goal_must_be_cleared_before_replacement() {
    let mut state = create(1, true);
    assert_eq!(state.goal.as_ref().unwrap().allocated_rounds, 0);
    assert!(state.goal.as_ref().unwrap().reservation.is_none());
    state
        .apply(GoalAction::Cancel { id: id("task") }, 256, true)
        .unwrap();
    assert_eq!(state.goal.as_ref().unwrap().allocated_rounds, 0);
    assert!(state.goal.as_ref().unwrap().reservation.is_none());
    let replacement = GoalAction::Create {
        id: id("replacement"),
        objective: "Different task".into(),
        constraints: String::new(),
        max_rounds: Some(1),
    };
    assert!(state.apply(replacement.clone(), 256, true).is_err());
    assert_eq!(state.goal.as_ref().unwrap().id, id("task"));
    state
        .apply(GoalAction::Clear { id: id("task") }, 256, true)
        .unwrap();
    state.apply(replacement, 256, true).unwrap();
    assert_eq!(state.goal.unwrap().id, id("replacement"));
}

#[test]
fn edits_preserve_charged_inputs_and_use_the_new_objective_and_budget_for_later_rounds() {
    let mut state = reserved(2);
    report(&mut state, GoalReportKind::Complete);
    let prior = state.clone();
    assert!(
        state
            .apply(
                GoalAction::Edit {
                    id: id("task"),
                    objective: Some("invalid".into()),
                    max_rounds: Some(0)
                },
                256,
                false
            )
            .is_err()
    );
    assert_eq!(state, prior);
    let original = state.goal.as_ref().unwrap().reservation.clone().unwrap();
    state
        .apply(
            GoalAction::Edit {
                id: id("task"),
                objective: Some("Revised task".into()),
                max_rounds: Some(20),
            },
            256,
            false,
        )
        .unwrap();
    state.validate().unwrap();
    let goal = state.goal.as_mut().unwrap();
    assert_eq!(goal.phase, GoalPhase::Active);
    assert_eq!(goal.allocated_rounds, 1);
    let reservation = goal.reservation.as_ref().unwrap();
    assert_eq!(reservation.message_id, original.message_id);
    assert_eq!(reservation.input_sha256, original.input_sha256);
    assert!(goal.report.is_none());
    assert!(
        goal.record_report(prior.goal.unwrap().report.unwrap())
            .is_err()
    );
    goal.settle(&original.message_id, terminal(RoundOutcome::Completed))
        .unwrap();
    let text = goal.reserve().unwrap();
    assert!(text.contains("2/20") && text.contains("Revised task"));
    state
        .apply(
            GoalAction::Edit {
                id: id("task"),
                objective: None,
                max_rounds: Some(1),
            },
            256,
            false,
        )
        .unwrap();
    let goal = state.goal.as_mut().unwrap();
    let message = goal.reservation.as_ref().unwrap().message_id.clone();
    goal.settle(&message, terminal(RoundOutcome::Completed))
        .unwrap();
    assert_eq!(goal.phase, GoalPhase::Blocked);
    assert_eq!(goal.allocated_rounds, 2);
    assert_eq!(goal.block_code.as_deref(), Some("round-limit"));
    assert!(
        state
            .apply(GoalAction::Resume { id: id("task") }, 256, false)
            .is_err()
    );
}
fn report(state: &mut GoalState, kind: GoalReportKind) {
    state
        .goal
        .as_mut()
        .unwrap()
        .record_report(GoalReport {
            kind,
            evidence: "Tests passed; source and artifacts are attached".into(),
            source_turn: TurnId::new("source").unwrap(),
        })
        .unwrap();
}

#[test]
fn edits_reissue_an_unclaimed_round_once_per_attempt_with_the_same_budget() {
    let mut state = reserved(1);
    let mut messages = std::collections::BTreeSet::new();
    let mut receipts = std::collections::BTreeSet::new();
    for attempt in 0..2 {
        let before = state.goal.as_ref().unwrap().reservation.clone().unwrap();
        assert_eq!((before.round, before.attempt), (1, attempt));
        assert!(messages.insert(before.message_id.clone()));
        assert!(receipts.insert(before.request_id.unwrap()));
        state
            .apply(
                GoalAction::Edit {
                    id: id("task"),
                    objective: Some("Revised".into()),
                    max_rounds: None,
                },
                1,
                false,
            )
            .unwrap();
        let goal = state.goal.as_mut().unwrap();
        goal.settle(&before.message_id, RoundSettlement::ContinuationDisarmed)
            .unwrap();
        let cold: GoalState = serde_json::from_slice(&serde_json::to_vec(&state).unwrap()).unwrap();
        cold.validate().unwrap();
        state = cold;
        let goal = state.goal.as_mut().unwrap();
        let next = goal.next_reservation().unwrap();
        assert_eq!(next.input.round, 1);
        assert_eq!(goal.reserve().unwrap(), next.input.text);
        assert!(next.input.text.contains("round 1/1") && next.input.text.contains("Revised"));
    }
    let goal = state.goal.as_mut().unwrap();
    let latest = goal.reservation.clone().unwrap();
    assert!(messages.insert(latest.message_id.clone()));
    assert!(receipts.insert(latest.request_id.unwrap()));
    goal.settle(&latest.message_id, terminal(RoundOutcome::Completed))
        .unwrap();
    assert_eq!((goal.phase, goal.allocated_rounds), (GoalPhase::Blocked, 1));
    assert!(!goal.has_round_capacity());

    let mut superseded = reserved(1);
    let cancelled = superseded
        .goal
        .as_ref()
        .unwrap()
        .reservation
        .as_ref()
        .unwrap()
        .message_id
        .clone();
    superseded
        .goal
        .as_mut()
        .unwrap()
        .settle(&cancelled, RoundSettlement::Discarded)
        .unwrap();
    superseded
        .apply(
            GoalAction::Edit {
                id: id("task"),
                objective: Some("Changed after discard".into()),
                max_rounds: None,
            },
            1,
            false,
        )
        .unwrap();
    assert!(!superseded.goal.unwrap().has_round_capacity());
}

#[test]
fn proposed_allocation_is_frozen_and_cannot_refund_or_double_allocate() {
    let mut state = create(2, true);
    let goal = state.goal.as_mut().unwrap();
    assert_eq!(goal.allocated_rounds, 0);
    let input = goal.reserve().unwrap();
    let reservation = goal.reservation.clone().unwrap();
    assert!(input.contains(&goal.objective));
    assert!(input.contains(&goal.constraints));
    assert!(input.contains("round 1/2"));
    assert!(!input.contains("report_goal"));
    assert!(goal.reserve().is_err());
    assert_eq!(goal.reservation.as_ref().unwrap(), &reservation);
    let encoded = serde_json::to_vec(&state).unwrap();
    let mut reopened: GoalState = serde_json::from_slice(&encoded).unwrap();
    reopened.validate().unwrap();
    reopened
        .apply(GoalAction::Pause { id: id("task") }, 256, false)
        .unwrap();
    reopened
        .goal
        .as_mut()
        .unwrap()
        .settle(&reservation.message_id, RoundSettlement::Discarded)
        .unwrap();
    reopened
        .apply(GoalAction::Resume { id: id("task") }, 256, false)
        .unwrap();
    let goal = reopened.goal.as_mut().unwrap();
    goal.reserve().unwrap();
    assert_eq!(goal.allocated_rounds, 2);
    assert_ne!(
        goal.reservation.as_ref().unwrap().message_id,
        reservation.message_id
    );
    let second = goal.reservation.as_ref().unwrap().message_id.clone();
    goal.settle(&second, terminal(RoundOutcome::Completed))
        .unwrap();
    assert_eq!(goal.phase, GoalPhase::Blocked);
    assert!(
        reopened
            .apply(GoalAction::Resume { id: id("task") }, 256, false)
            .is_err()
    );
}

#[test]
fn draft_and_durable_creation_allocate_nothing_until_reservation() {
    for draft in [false, true] {
        let mut state = create(1, draft);
        let goal = state.goal.as_mut().unwrap();
        assert_eq!(goal.allocated_rounds, 0);
        assert!(goal.reservation.is_none());
        goal.phase = GoalPhase::Paused;
        assert!(goal.reserve().is_err());
        assert_eq!(goal.allocated_rounds, 0);
        goal.phase = GoalPhase::Active;
        goal.reserve().unwrap();
        let original = goal.reservation.clone().unwrap();
        assert_eq!(goal.allocated_rounds, 1);
        assert!(goal.reserve().is_err());
        assert_eq!(goal.reservation.as_ref(), Some(&original));
        state.validate().unwrap();
    }
}

#[test]
fn settlement_of_an_earlier_round_preserves_the_users_pause() {
    for (outcome, kind, phase) in [
        (
            RoundOutcome::Failed,
            GoalReportKind::Complete,
            GoalPhase::Paused,
        ),
        (
            RoundOutcome::Completed,
            GoalReportKind::Blocked,
            GoalPhase::Paused,
        ),
        (
            RoundOutcome::Completed,
            GoalReportKind::Complete,
            GoalPhase::Completed,
        ),
    ] {
        let mut state = reserved(2);
        report(&mut state, kind);
        state
            .apply(GoalAction::Pause { id: id("task") }, 2, false)
            .unwrap();
        let goal = state.goal.as_mut().unwrap();
        let message = goal.reservation.as_ref().unwrap().message_id.clone();
        let settlement = terminal(outcome);
        goal.settle(&message, settlement.clone()).unwrap();
        assert_eq!(goal.phase, phase);
        assert_eq!(
            goal.reservation.as_ref().unwrap().settlement.as_ref(),
            Some(&settlement)
        );
        if phase == GoalPhase::Paused {
            assert_eq!(goal.reason.as_deref(), Some("Paused by the user"));
        }
    }
}

#[test]
fn cancelling_after_a_failed_turn_preserves_the_blocker() {
    let mut state = reserved(2);
    let goal = state.goal.as_mut().unwrap();
    let message = goal.reservation.as_ref().unwrap().message_id.clone();
    goal.settle(&message, terminal(RoundOutcome::Failed))
        .unwrap();
    let blocked = state.clone();
    state
        .apply(GoalAction::Cancel { id: id("task") }, 2, false)
        .unwrap();
    assert_eq!(state, blocked);
}

#[test]
fn failure_overrides_a_model_completion_claim_and_retains_its_source() {
    for outcome in [
        RoundOutcome::Blocked,
        RoundOutcome::Failed,
        RoundOutcome::PartialFailed,
        RoundOutcome::Interrupted,
        RoundOutcome::MaxTokens,
    ] {
        let mut state = reserved(2);
        report(&mut state, GoalReportKind::Complete);
        let goal = state.goal.as_mut().unwrap();
        assert_eq!(goal.phase, GoalPhase::Active);
        let message = goal.reservation.as_ref().unwrap().message_id.clone();
        goal.settle(&message, terminal(outcome)).unwrap();
        assert_eq!(goal.phase, GoalPhase::Blocked);
        assert_eq!(goal.report.as_ref().unwrap().kind, GoalReportKind::Complete);
        assert_eq!(goal.allocated_rounds, 1);
        state.validate().unwrap();
    }
}

#[test]
fn report_verification_checks_source_settlement_for_every_disposition() {
    for kind in [
        GoalReportKind::Complete,
        GoalReportKind::Blocked,
        GoalReportKind::Pause,
    ] {
        for outcome in [
            RoundOutcome::Completed,
            RoundOutcome::Cancelled,
            RoundOutcome::Blocked,
            RoundOutcome::Failed,
            RoundOutcome::PartialFailed,
            RoundOutcome::Interrupted,
            RoundOutcome::MaxTokens,
        ] {
            for matching in [true, false] {
                let mut state = reserved(2);
                report(&mut state, kind);
                let goal = state.goal.as_mut().unwrap();
                assert!(
                    goal.verified_report().is_none(),
                    "an unsettled report is only a claim"
                );
                let message = goal.reservation.as_ref().unwrap().message_id.clone();
                let turn_id = TurnId::new(if matching { "source" } else { "foreign" }).unwrap();
                goal.settle(&message, RoundSettlement::Turn { turn_id, outcome })
                    .unwrap();
                assert_eq!(
                    goal.verified_report().is_some(),
                    matching && outcome == RoundOutcome::Completed
                );
                assert!(
                    goal.report.is_some(),
                    "unverified claims remain diagnostic evidence"
                );
                state.validate().unwrap();
            }
        }
        for settlement in [RoundSettlement::Abandoned, RoundSettlement::Discarded] {
            let mut state = reserved(2);
            report(&mut state, kind);
            let goal = state.goal.as_mut().unwrap();
            let reservation = goal.reservation.as_ref().unwrap();
            let message = reservation.message_id.clone();
            let digest = reservation.input_sha256.clone();
            goal.settle(&message, settlement).unwrap();
            assert_eq!(goal.reservation.as_ref().unwrap().input_sha256, digest);
            assert!(goal.verified_report().is_none());
        }
    }
}

#[test]
fn only_matching_successful_turn_verifies_completion_and_cancellation_pauses() {
    let mut state = reserved(2);
    report(&mut state, GoalReportKind::Complete);
    let goal = state.goal.as_mut().unwrap();
    let message = goal.reservation.as_ref().unwrap().message_id.clone();
    goal.settle(&message, terminal(RoundOutcome::Completed))
        .unwrap();
    assert_eq!(goal.phase, GoalPhase::Completed);
    goal.settle(&message, terminal(RoundOutcome::Completed))
        .unwrap();
    assert!(
        goal.settle(&message, terminal(RoundOutcome::Failed))
            .is_err()
    );
    assert!(
        state
            .apply(GoalAction::Resume { id: id("task") }, 256, false)
            .is_err()
    );

    let mut state = reserved(2);
    report(&mut state, GoalReportKind::Complete);
    let goal = state.goal.as_mut().unwrap();
    let message = goal.reservation.as_ref().unwrap().message_id.clone();
    goal.settle(
        &message,
        RoundSettlement::Turn {
            turn_id: TurnId::new("different").unwrap(),
            outcome: RoundOutcome::Completed,
        },
    )
    .unwrap();
    assert_eq!(goal.phase, GoalPhase::Active);
    goal.reserve().unwrap();
    let message = goal.reservation.as_ref().unwrap().message_id.clone();
    goal.settle(&message, terminal(RoundOutcome::Cancelled))
        .unwrap();
    assert_eq!(goal.phase, GoalPhase::Paused);
}

#[test]
fn resume_preserves_frozen_round_count_and_pause_survives_no_report_success() {
    let mut state = reserved(3);
    let original = state.goal.as_ref().unwrap().max_rounds;
    let tiny = 1;
    state
        .apply(GoalAction::Pause { id: id("task") }, tiny, false)
        .unwrap();
    let goal = state.goal.as_mut().unwrap();
    let message = goal.reservation.as_ref().unwrap().message_id.clone();
    goal.settle(&message, terminal(RoundOutcome::Completed))
        .unwrap();
    assert_eq!(goal.phase, GoalPhase::Paused);
    state
        .apply(GoalAction::Resume { id: id("task") }, tiny, false)
        .unwrap();
    assert_eq!(state.goal.as_ref().unwrap().max_rounds, original);
    assert_eq!(state.goal.as_ref().unwrap().allocated_rounds, 1);
}

#[test]
fn optional_creation_freezes_configured_default_and_rejects_unsafe_rounds() {
    for (requested, default, expected) in [
        (None, 256, 256),
        (None, 17, 17),
        (Some(9), 256, 9),
        (Some(9_007_199_254_740_991), 256, 9_007_199_254_740_991),
    ] {
        let mut state = GoalState::default();
        state
            .apply(
                GoalAction::Create {
                    id: id("task"),
                    objective: "Do the work".into(),
                    constraints: String::new(),
                    max_rounds: requested,
                },
                default,
                true,
            )
            .unwrap();
        assert_eq!(state.goal.unwrap().max_rounds, expected);
    }
    for rounds in [0, 9_007_199_254_740_992, u64::MAX] {
        let mut state = GoalState::default();
        assert!(
            state
                .apply(
                    GoalAction::Create {
                        id: id("task"),
                        objective: "Do the work".into(),
                        constraints: String::new(),
                        max_rounds: Some(rounds)
                    },
                    256,
                    true
                )
                .is_err()
        );
        assert!(state.goal.is_none());
    }
}

#[test]
fn codec_rejects_tampered_reservation_and_unverified_complete_state() {
    let state = reserved(2);
    for change in [
        "message_id",
        "request_id",
        "objective",
        "input_sha256",
        "phase",
        "round",
        "count",
    ] {
        let mut value = serde_json::to_value(&state).unwrap();
        let goal = &mut value["goal"];
        match change {
            "phase" => goal["phase"] = "completed".into(),
            "objective" => goal["objective"] = "different frozen input".into(),
            "round" => goal["reservation"]["round"] = 2.into(),
            "count" => goal["allocated_rounds"] = 0.into(),
            field => goal["reservation"][field] = "forged".into(),
        }
        let decoded: GoalState = serde_json::from_value(value).unwrap();
        assert!(decoded.validate().is_err(), "accepted {change}");
    }
}

#[test]
fn allocated_input_validates_at_maximum_text_sizes_and_rejects_an_invalid_goal() {
    let mut state = create(1, false);
    let goal = state.goal.as_mut().unwrap();
    goal.objective = "o".repeat(8192);
    goal.constraints = "c".repeat(4096);
    let mut invalid = goal.clone();
    invalid.constraints.push('x');
    assert!(invalid.reserve().is_err());
    let prepared = goal.next_reservation().unwrap();
    assert_eq!(goal.allocated_rounds, 0);
    assert!(goal.reservation.is_none());
    let input = goal.reserve().unwrap();
    let admitted = goal.reservation.as_ref().unwrap();
    assert_eq!(input, prepared.input.text);
    assert_eq!(admitted.message_id, prepared.input.message_id);
    assert_eq!(admitted.request_id.as_ref(), Some(&prepared.request_id));
    assert_eq!(admitted.input_sha256, prepared.input_sha256);
    assert!(goal.next_reservation().is_err());
    state.validate().unwrap();
}

#[test]
fn a_settled_blocked_goal_fits_the_public_projection_at_escaped_text_limits() {
    let mut state = create(2, false);
    let goal = state.goal.as_mut().unwrap();
    goal.objective = "\\".repeat(8192);
    goal.constraints = "\"".repeat(4096);
    goal.reserve().unwrap();
    let reservation = goal.reservation.as_ref().unwrap();
    let message = reservation.message_id.clone();
    let digest = reservation.input_sha256.clone();
    let turn = TurnId::new("source").unwrap();
    goal.record_report(GoalReport {
        kind: GoalReportKind::Blocked,
        evidence: "\\".repeat(4096),
        source_turn: turn.clone(),
    })
    .unwrap();
    goal.settle(
        &message,
        RoundSettlement::Turn {
            turn_id: turn,
            outcome: RoundOutcome::Completed,
        },
    )
    .unwrap();
    let view = rsi_agent_session_protocol::ProjectionValue::encode(&state).unwrap();
    let receipt = &view.value()["goal"]["reservation"];
    assert_eq!(receipt["message_id"], message.as_str());
    assert_eq!(receipt["input_sha256"], digest);
    assert!(receipt.get("input").is_none());
    let reopened: GoalState = serde_json::from_value(view.value().clone()).unwrap();
    reopened.validate().unwrap();
    assert_eq!(reopened, state);
    assert_eq!(
        reopened
            .goal
            .unwrap()
            .verified_report()
            .unwrap()
            .evidence
            .len(),
        4096
    );
}

#[test]
fn stale_identity_oversized_text_and_model_control_verbs_are_rejected() {
    let mut state = create(2, false);
    assert!(
        state
            .apply(GoalAction::Pause { id: id("other") }, 256, false)
            .is_err()
    );
    for action in ["complete", "blocked", "reserve", "settle"] {
        assert!(
            serde_json::from_value::<GoalAction>(serde_json::json!({"action":action,"id":"task"}))
                .is_err()
        );
    }
    let mut empty = GoalState::default();
    assert!(
        empty
            .apply(
                GoalAction::Create {
                    id: id("huge"),
                    objective: "x".repeat(8193),
                    constraints: String::new(),
                    max_rounds: Some(1)
                },
                256,
                false
            )
            .is_err()
    );
    assert_eq!(empty, GoalState::default());
}
