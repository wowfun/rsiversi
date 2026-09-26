use rsi_agent_schedule::{ScheduleRule, ScheduleState};
use rsi_agent_session_protocol::DomainRequestId;
fn id(value: &str) -> DomainRequestId {
    DomainRequestId::new(value).unwrap()
}
#[test]
fn delayed_fixed_rate_work_coalesces_and_keeps_its_original_anchor() {
    let mut state = ScheduleState::default();
    state
        .create(
            id("every"),
            "Check the build".into(),
            ScheduleRule::Every {
                interval_ms: 300_000,
            },
            1000,
        )
        .unwrap();
    assert_eq!(state.allocated_rounds, 0);
    assert!(state.reserve(300_999).is_err());
    let first = state.reserve(1_000_000).unwrap();
    assert_eq!(state.reservation.as_ref().unwrap().occurrence_ms, 901_000);
    assert_eq!(state.reminders[0].due_ms, 1_201_000);
    assert_eq!(state.allocated_rounds, 1);
    assert!(state.reserve(2_000_000).is_err());
    state.settle(&first.message_id).unwrap();
    assert_eq!(state.next_due(), Some(1_201_000));
    state.validate().unwrap();
}
#[test]
fn one_shot_is_charged_once_and_selected_resume_cannot_replenish_it() {
    let mut state = ScheduleState::default();
    state
        .create(
            id("once"),
            "Check".into(),
            ScheduleRule::At { at_ms: 2000 },
            1000,
        )
        .unwrap();
    let input = state.reserve(9000).unwrap();
    state.settle(&input.message_id).unwrap();
    assert!(state.next_due().is_none());
    assert!(state.resume(&[id("once")]).is_err());
    state.delete(&id("once")).unwrap();
    assert_eq!(state.allocated_rounds, 1);
    state.validate().unwrap();
}
#[test]
fn resume_selects_pending_intent_without_losing_unselected_one_shots() {
    let mut state = ScheduleState::default();
    for name in ["first", "second"] {
        state
            .create(
                id(name),
                "Check".into(),
                ScheduleRule::After { delay_ms: 1 },
                1000,
            )
            .unwrap();
    }
    state.resume(&[id("first")]).unwrap();
    assert!(!state.reminders[1].active);
    state.resume(&[id("second")]).unwrap();
    assert!(state.reminders[1].active && !state.reminders[0].active);
    assert_eq!(state.allocated_rounds, 0);
}
#[test]
fn lifetime_allowance_survives_deletion_recreation_and_serialization() {
    let mut state = ScheduleState::default();
    for round in 1..=100 {
        let reminder = id(&format!("round-{round}"));
        state
            .create(
                reminder.clone(),
                "Check".into(),
                ScheduleRule::After { delay_ms: 1 },
                1000,
            )
            .unwrap();
        let input = state.reserve(2000).unwrap();
        assert_eq!(input.round, round);
        state.settle(&input.message_id).unwrap();
        state.delete(&reminder).unwrap();
        state = serde_json::from_slice(&serde_json::to_vec(&state).unwrap()).unwrap();
        state.validate().unwrap();
    }
    assert!(
        state
            .create(
                id("extra"),
                "Check".into(),
                ScheduleRule::After { delay_ms: 1 },
                1000
            )
            .is_err()
    );
    assert_eq!(state.allocated_rounds, 100);
}
#[test]
fn bounds_and_overflow_preserve_the_original_candidate() {
    let mut state = ScheduleState::default();
    for rule in [
        ScheduleRule::After { delay_ms: 0 },
        ScheduleRule::At { at_ms: 1000 },
        ScheduleRule::Every {
            interval_ms: 299_999,
        },
        ScheduleRule::After { delay_ms: u64::MAX },
    ] {
        assert!(state.create(id("bad"), "Check".into(), rule, 1000).is_err());
        assert_eq!(state, ScheduleState::default());
    }
    assert!(
        state
            .create(
                id("huge"),
                "x".repeat(2049),
                ScheduleRule::After { delay_ms: 1 },
                1000
            )
            .is_err()
    );
    for index in 0..16 {
        state
            .create(
                id(&format!("r-{index}")),
                "Check".into(),
                ScheduleRule::After { delay_ms: 1 },
                1000,
            )
            .unwrap();
    }
    assert!(
        state
            .create(
                id("extra"),
                "Check".into(),
                ScheduleRule::After { delay_ms: 1 },
                1000
            )
            .is_err()
    );
    let mut state = ScheduleState::default();
    state
        .create(
            id("overflow"),
            "Check".into(),
            ScheduleRule::Every {
                interval_ms: 300_000,
            },
            u64::MAX - 300_001,
        )
        .unwrap();
    let before = state.clone();
    assert!(state.reserve(u64::MAX).is_err());
    assert_eq!(state, before);
}
#[test]
fn codec_rejects_tampered_frozen_input_and_inconsistent_anchors() {
    let mut state = ScheduleState::default();
    state
        .create(
            id("once"),
            "Check".into(),
            ScheduleRule::After { delay_ms: 1 },
            1000,
        )
        .unwrap();
    state.reserve(2000).unwrap();
    let mut tampered = state.clone();
    tampered.reservation.as_mut().unwrap().input.text = "forged".into();
    assert!(tampered.validate().is_err());
    let mut tampered = state.clone();
    tampered.reminders[0].active = true;
    assert!(tampered.validate().is_err());
    let mut tampered = state;
    tampered.allocated_rounds = 101;
    assert!(tampered.validate().is_err());
}

#[test]
fn resume_can_reconcile_the_last_accepted_round_without_replenishing_it() {
    let mut state = ScheduleState::default();
    for round in 1..=100 {
        let reminder = id(&format!("round-{round}"));
        state
            .create(
                reminder.clone(),
                "Check".into(),
                ScheduleRule::After { delay_ms: 1 },
                1000,
            )
            .unwrap();
        let input = state.reserve(2000).unwrap();
        state.resume(std::slice::from_ref(&reminder)).unwrap();
        assert!(!state.reminders[0].active);
        assert!(state.next_due().is_none());
        state.validate().unwrap();
        state.settle(&input.message_id).unwrap();
        assert!(state.resume(std::slice::from_ref(&reminder)).is_err());
        state.delete(&reminder).unwrap();
    }
    assert_eq!(state.allocated_rounds, 100);
}
