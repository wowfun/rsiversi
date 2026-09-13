use rsi_agent_goal::{
    GoalAction, GoalAllowance, GoalPhase, GoalReport, GoalReportKind, GoalState, RoundOutcome,
    RoundSettlement,
};
use rsi_agent_session_protocol::{DomainRequestId, TurnBudget, TurnId};

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
                max_rounds,
            },
            &TurnBudget::default(),
            draft,
        )
        .unwrap();
    state
}
fn terminal(outcome: RoundOutcome) -> RoundSettlement {
    RoundSettlement::Turn {
        turn_id: TurnId::new("source").unwrap(),
        outcome,
    }
}

#[test]
fn cancelling_an_unpublished_allocation_allows_replacement_without_execution_or_refund() {
    let mut state = create(1, true);
    let original = state.goal.as_ref().unwrap().reservation.clone().unwrap();
    state
        .apply(
            GoalAction::Cancel { id: id("task") },
            &TurnBudget::default(),
            true,
        )
        .unwrap();
    let goal = state.goal.as_ref().unwrap();
    assert_eq!(goal.allocated_rounds, 1);
    assert_eq!(
        goal.reservation.as_ref().unwrap().message_id,
        original.message_id
    );
    assert_eq!(
        goal.reservation.as_ref().unwrap().settlement,
        Some(RoundSettlement::Abandoned)
    );
    assert!(
        state
            .apply(
                GoalAction::Resume { id: id("task") },
                &TurnBudget::default(),
                true
            )
            .is_err()
    );
    state
        .apply(
            GoalAction::Create {
                id: id("replacement"),
                objective: "Different task".into(),
                constraints: String::new(),
                max_rounds: 1,
            },
            &TurnBudget::default(),
            true,
        )
        .unwrap();
    assert_eq!(state.goal.unwrap().id, id("replacement"));
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
fn allocation_is_frozen_before_acceptance_and_cannot_refund_or_double_allocate() {
    let mut state = create(2, true);
    let goal = state.goal.as_mut().unwrap();
    assert_eq!(goal.allocated_rounds, 1);
    goal.reserve().unwrap();
    let reservation = goal.reservation.clone().unwrap();
    assert!(goal.reserve().is_err());
    assert_eq!(goal.reservation.as_ref().unwrap(), &reservation);
    let encoded = serde_json::to_vec(&state).unwrap();
    let mut reopened: GoalState = serde_json::from_slice(&encoded).unwrap();
    reopened.validate().unwrap();
    reopened
        .apply(
            GoalAction::Pause { id: id("task") },
            &TurnBudget::default(),
            false,
        )
        .unwrap();
    reopened
        .goal
        .as_mut()
        .unwrap()
        .settle(&reservation.message_id, RoundSettlement::Discarded)
        .unwrap();
    reopened
        .apply(
            GoalAction::Resume { id: id("task") },
            &TurnBudget::default(),
            false,
        )
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
            .apply(
                GoalAction::Resume { id: id("task") },
                &TurnBudget::default(),
                false
            )
            .is_err()
    );
}

#[test]
fn binding_published_baseline_preserves_the_exhausted_first_allocation() {
    let mut state = create(1, true);
    let goal = state.goal.as_mut().unwrap();
    let original = goal.reservation.clone().unwrap();
    goal.phase = GoalPhase::Paused;
    assert!(goal.reserve().is_err());
    assert_eq!(goal.reservation.as_ref(), Some(&original));
    goal.phase = GoalPhase::Active;
    goal.reserve().unwrap();
    let bound = goal.reservation.clone().unwrap();
    assert_eq!(bound.input(&goal.id), original.input(&goal.id));
    assert_eq!(goal.allocated_rounds, 1);
    assert_eq!(
        bound.request_id,
        Some(rsi_agent_goal::round_request_id(&goal.id, 1, "reserve").unwrap())
    );
    assert!(goal.reserve().is_err());
    assert_eq!(goal.reservation.as_ref(), Some(&bound));
    state.validate().unwrap();
}

#[test]
fn failure_overrides_a_model_completion_claim_and_retains_its_source() {
    for outcome in [
        RoundOutcome::Failed,
        RoundOutcome::PartialFailed,
        RoundOutcome::Interrupted,
        RoundOutcome::BudgetExceeded,
    ] {
        let mut state = create(2, true);
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
fn only_matching_successful_turn_verifies_completion_and_cancellation_pauses() {
    let mut state = create(2, true);
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
            .apply(
                GoalAction::Resume { id: id("task") },
                &TurnBudget::default(),
                false
            )
            .is_err()
    );

    let mut state = create(2, true);
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
fn resume_preserves_frozen_budget_and_pause_survives_no_report_success() {
    let mut state = create(3, true);
    let original = state.goal.as_ref().unwrap().turn_budget.clone();
    let tiny = TurnBudget::new(1, 1, 1, 1, 1).unwrap();
    state
        .apply(GoalAction::Pause { id: id("task") }, &tiny, false)
        .unwrap();
    let goal = state.goal.as_mut().unwrap();
    let message = goal.reservation.as_ref().unwrap().message_id.clone();
    goal.settle(&message, terminal(RoundOutcome::Completed))
        .unwrap();
    assert_eq!(goal.phase, GoalPhase::Paused);
    state
        .apply(GoalAction::Resume { id: id("task") }, &tiny, false)
        .unwrap();
    assert_eq!(state.goal.as_ref().unwrap().turn_budget, original);
    assert_eq!(state.goal.as_ref().unwrap().allocated_rounds, 1);
}

#[test]
fn all_five_allowance_dimensions_are_checked_and_zero_is_rejected() {
    let budget = TurnBudget::new(7, 11, 13, 17, 19).unwrap();
    assert_eq!(
        GoalAllowance::new(3, &budget).unwrap(),
        GoalAllowance {
            elapsed_ms: 21,
            provider_attempts: 33,
            tool_calls: 39,
            generated_records: 51,
            generated_record_bytes: 57,
        }
    );
    assert!(GoalAllowance::new(0, &budget).is_err());
    for dimensions in [
        [2, 1, 1, 1, 1],
        [1, 2, 1, 1, 1],
        [1, 1, 2, 1, 1],
        [1, 1, 1, 2, 1],
        [1, 1, 1, 1, 2],
    ] {
        let budget = TurnBudget::new(
            dimensions[0],
            dimensions[1],
            dimensions[2],
            dimensions[3],
            dimensions[4],
        )
        .unwrap();
        assert!(GoalAllowance::new(u64::MAX, &budget).is_err());
    }
}

#[test]
fn codec_rejects_tampered_reservation_and_unverified_complete_state() {
    let state = create(2, true);
    for change in [
        "message_id",
        "input",
        "input_sha256",
        "phase",
        "round",
        "count",
    ] {
        let mut value = serde_json::to_value(&state).unwrap();
        let goal = &mut value["goal"];
        match change {
            "phase" => goal["phase"] = "completed".into(),
            "round" => goal["reservation"]["round"] = 2.into(),
            "count" => goal["allocated_rounds"] = 0.into(),
            field => goal["reservation"][field] = "forged".into(),
        }
        let decoded: GoalState = serde_json::from_value(value).unwrap();
        assert!(decoded.validate().is_err(), "accepted {change}");
    }
}

#[test]
fn stale_identity_oversized_text_and_model_control_verbs_are_rejected() {
    let mut state = create(2, false);
    assert!(
        state
            .apply(
                GoalAction::Pause { id: id("other") },
                &TurnBudget::default(),
                false
            )
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
                    max_rounds: 1
                },
                &TurnBudget::default(),
                false
            )
            .is_err()
    );
    assert_eq!(empty, GoalState::default());
}
