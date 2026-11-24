use super::*;
use serde_json::{Value, json};

#[test]
fn goal_history_contains_the_completed_state_without_copying_the_reserved_prompt() {
    use rsi_agent_goal::{GOAL_CODEC_VERSION, GOAL_DOMAIN, GoalAction, GoalCompleter, GoalState};
    use rsi_agent_session_protocol::{
        DomainIdentity, DomainRequestId, DomainRevision, DomainSnapshot, DomainStateUpdate,
        DomainStateValue,
    };
    let update = |previous, state: &GoalState| {
        DomainStateUpdate::new(
            DomainRevision::new(previous),
            DomainSnapshot::new(
                DomainIdentity::new(GOAL_DOMAIN, GOAL_CODEC_VERSION).unwrap(),
                DomainStateValue::new(json!(state)).unwrap(),
            ),
        )
        .unwrap()
    };
    let baseline = |state: &GoalState| {
        DomainStateCommit::new(None, DomainMutationSource::Baseline, vec![update(0, state)])
            .unwrap()
    };
    let mut state = GoalState::default();
    assert!(goal_change(&baseline(&state)).unwrap().is_none());
    let id = DomainRequestId::new("goal-1").unwrap();
    state
        .apply(
            GoalAction::Create {
                id: id.clone(),
                objective: "Review the draft".into(),
                constraints: "only in the round prompt".into(),
                max_rounds: Some(2),
            },
            2,
            true,
        )
        .unwrap();
    state.goal.as_mut().unwrap().reserve().unwrap();
    state
        .apply(
            GoalAction::Block {
                id: id.clone(),
                code: "needs-input".into(),
                message: "Choose an account".into(),
            },
            2,
            true,
        )
        .unwrap();
    let blocked = serde_json::to_value(goal_change(&baseline(&state)).unwrap()).unwrap();
    assert_eq!(blocked["current"]["block_code"], "needs-input");
    assert_eq!(blocked["current"]["reason"], "Choose an account");
    state
        .apply(
            GoalAction::Complete {
                id,
                source: GoalCompleter::Owner,
            },
            2,
            true,
        )
        .unwrap();
    assert_eq!(
        serde_json::to_value(goal_change(&baseline(&state)).unwrap()).unwrap(),
        json!({"current":{"id":"goal-1","objective":"Review the draft","phase":"completed","max_rounds":2,"reason":null}})
    );
    let invocation = SessionCommandInvocation {
        command: rsi_agent_session_protocol::ContributionId::new(rsi_agent_goal::GOAL_COMMAND)
            .unwrap(),
        request_id: DomainRequestId::new("finish").unwrap(),
        expected_revision: rsi_agent_session_protocol::CommandRevision::Durable { control_seq: 3 },
        arguments: rsi_agent_session_protocol::CommandArguments::new(json!({
            "action":"complete","id":"goal-1","source":{"kind":"owner"},"published_at_ms":12
        }))
        .unwrap(),
    };
    let commit = DomainStateCommit::new(
        Some(invocation.request_id.clone()),
        DomainMutationSource::Command { invocation },
        vec![update(1, &state)],
    )
    .unwrap();
    let encoded = serde_json::to_value(ControlEvent {
        seq: 4,
        after_fact_seq: 0,
        time: 15,
        body: ControlBody::Goal,
        goal: goal_change(&commit).unwrap(),
    })
    .unwrap();
    assert_eq!(encoded["time"], 15);
    assert_eq!(
        encoded["goal"]["command"],
        json!({"action":"complete","request_id":"finish","published_at_ms":12})
    );
}

#[test]
fn model_command_origin_distinguishes_identical_values_and_aggregate_baselines() {
    use rsi_agent_session_protocol::{
        CommandArguments, CommandRevision, ContributionId, DomainIdentity, DomainRequestId,
        DomainRevision, DomainSnapshot, DomainStateUpdate, DomainStateValue,
        SessionCommandInvocation,
    };
    let update = DomainStateUpdate::new(
        DomainRevision::new(1),
        DomainSnapshot::new(
            DomainIdentity::new(rsi_agent_model_selection::DOMAIN, 1).unwrap(),
            DomainStateValue::new(json!({"model":{"deployment":"native","model":"chosen"}}))
                .unwrap(),
        ),
    )
    .unwrap();
    let mut results = Vec::new();
    for id in ["first", "second"] {
        let invocation = SessionCommandInvocation {
            command: ContributionId::new(rsi_agent_model_selection::COMMAND).unwrap(),
            request_id: DomainRequestId::new(id).unwrap(),
            expected_revision: CommandRevision::Durable { control_seq: 3 },
            arguments: CommandArguments::new(update.snapshot().state().value().clone()).unwrap(),
        };
        let commit = DomainStateCommit::new(
            Some(invocation.request_id.clone()),
            DomainMutationSource::Command {
                invocation: invocation.clone(),
            },
            vec![update.clone()],
        )
        .unwrap();
        let value = serde_json::to_value(ControlEvent {
            seq: 4,
            after_fact_seq: 7,
            time: 10,
            goal: None,
            body: ControlBody::Model {
                selection: serde_json::from_value(update.snapshot().state().value().clone())
                    .unwrap(),
                origin: command_origin(4, &commit).unwrap(),
            },
        })
        .unwrap();
        assert_eq!(value["kind"], "model");
        assert_eq!(
            value["selection"],
            json!({"model":{"deployment":"native","model":"chosen"},"reasoning_effort":null})
        );
        assert_eq!(
            (
                value["seq"].as_u64(),
                value["after_fact_seq"].as_u64(),
                value["time"].as_u64()
            ),
            (Some(4), Some(7), Some(10))
        );
        assert_eq!(value["invocation"], json!(invocation));
        assert_eq!(
            value["receipt"],
            json!(SessionCommandReceipt::committed(4, &commit).unwrap())
        );
        assert!(command_origin(5, &commit).is_err());
        results.push(value);
    }
    assert_ne!(results[0]["receipt"], results[1]["receipt"]);
    let initial =
        DomainStateUpdate::new(DomainRevision::new(0), update.snapshot().clone()).unwrap();
    let baseline =
        DomainStateCommit::new(None, DomainMutationSource::Baseline, vec![initial]).unwrap();
    assert!(command_origin(1, &baseline).unwrap().is_none());
    let value = serde_json::to_value(ControlBody::Model {
        selection: serde_json::from_value(update.snapshot().state().value().clone()).unwrap(),
        origin: None,
    })
    .unwrap();
    assert_eq!(
        value,
        json!({"kind":"model","selection":{"model":{"deployment":"native","model":"chosen"},"reasoning_effort":null}})
    );
}

#[test]
fn mailbox_transitions_keep_their_wire_fields_without_internal_execution_metadata() {
    let slot = json!({"id":"old","timestamp_ms":1,"control_seq":1});
    for (source, expected) in [
        (
            json!({"type":"message_successor","predecessor_id":"old","successor_id":"new","slot":slot}),
            json!({"kind":"successor","predecessor_id":"old","successor_id":"new","slot":slot}),
        ),
        (
            json!({"type":"message_claimed","message_id":"message","activation_id":"activation","turn_id":"turn","step_id":"step","claimed_fact_seq":1}),
            json!({"kind":"claimed","message_id":"message","turn_id":"turn"}),
        ),
        (
            json!({"type":"message_promoted","message_id":"message"}),
            json!({"kind":"promoted","message_id":"message"}),
        ),
        (
            json!({"type":"message_discarded","message_id":"message","reason":"cancelled"}),
            json!({"kind":"discarded","message_id":"message"}),
        ),
    ] {
        let body: Body = serde_json::from_value(source).unwrap();
        assert_eq!(
            serde_json::to_value(mailbox_event(&body).unwrap()).unwrap(),
            expected
        );
    }
    assert!(mailbox_event(&Body::InboxPaused).is_none());
}

#[test]
fn acceptance_control_fragments_preserve_exact_multibyte_input() {
    use rsi_agent_session_protocol::{
        AgentMessage, AgentMessageContent, AgentMessageSource, MessageDelivery, MessageId,
        MessageOptions, MessageTarget, SessionId,
    };
    let message = AgentMessage {
        message_id: MessageId::new("large-context").unwrap(),
        source: AgentMessageSource::Human { attribution: None },
        content: vec![AgentMessageContent::Text {
            text: "界".repeat(100_000),
        }],
        options: MessageOptions::default(),
    };
    let body = Body::MessageAccepted {
        message: message.clone(),
        delivery: MessageDelivery::NextTurn,
        bound_turn_id: None,
        root_session_id: SessionId::new("root").unwrap(),
        target: MessageTarget::NextTurn,
        wake_required: true,
    };
    let metadata = mailbox_event(&body).unwrap();
    let decoded = serde_json::to_value(&metadata).unwrap();
    assert_eq!(decoded["message_id"], "large-context");
    assert_eq!(decoded["message"], json!(message));
    let encoded = serde_json::to_string(&vec![metadata]).unwrap();
    let mut offset = 0;
    let mut restored = String::new();
    while offset < encoded.len() {
        let fragment = super::super::session_history::fragment(&encoded, offset, 42).unwrap();
        assert_eq!(fragment["through_seq"], 42);
        restored.push_str(fragment["json"].as_str().unwrap());
        offset = usize::try_from(fragment["next_offset"].as_u64().unwrap()).unwrap();
    }
    assert_eq!(
        serde_json::from_str::<Value>(&restored).unwrap()[0]["message"],
        json!(message)
    );
}

#[test]
#[ignore = "opt-in isolated control serialization cost probe"]
fn large_control_projection_cost() {
    use rsi_agent_session_protocol::{
        AgentMessage, AgentMessageContent, AgentMessageSource, MessageDelivery, MessageId,
        MessageOptions, MessageTarget, SessionId,
    };
    let body = Body::MessageAccepted {
        message: AgentMessage {
            message_id: MessageId::new("cost").unwrap(),
            source: AgentMessageSource::Human { attribution: None },
            content: vec![AgentMessageContent::Text {
                text: "界\n".repeat(2 * 1024 * 1024),
            }],
            options: MessageOptions::default(),
        },
        delivery: MessageDelivery::NextTurn,
        bound_turn_id: None,
        root_session_id: SessionId::new("root").unwrap(),
        target: MessageTarget::NextTurn,
        wake_required: true,
    };
    let started = std::time::Instant::now();
    let mut bytes = 0;
    for _ in 0..16 {
        let value = mailbox_event(std::hint::black_box(&body)).unwrap();
        bytes = std::hint::black_box(serde_json::to_string(&value).unwrap()).len();
    }
    assert!(bytes > 8 * 1024 * 1024);
    eprintln!(
        "control_projection_probe iterations=16 encoded_bytes={bytes} elapsed_us={}",
        started.elapsed().as_micros()
    );
}
