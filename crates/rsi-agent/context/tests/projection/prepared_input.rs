use super::*;
use rsi_agent_session_protocol::{
    InputClaimRef, MessageAttribution, PreparedInput, PreparedInputMessage, PreparedInputRole,
    StepInputDecision,
};

fn initial() -> Vec<SessionFact> {
    facts(vec![
        SessionFactBody::TurnAccepted {
            turn_id: TurnId::new("input-turn").unwrap(),
            text: "original direct input".into(),
            model: None,
            reasoning_effort: None,
            sandbox: SandboxMode::WorkspaceWrite,
            require_approval: false,
        },
        SessionFactBody::StepStarted {
            turn_id: TurnId::new("input-turn").unwrap(),
            step_id: StepId::new("step").unwrap(),
        },
        SessionFactBody::InputMessageClaimed {
            turn_id: TurnId::new("input-turn").unwrap(),
            step_id: StepId::new("step").unwrap(),
            source: InputMessageSource::Human {
                message_id: MessageId::new("queued").unwrap(),
                attribution: None,
            },
            content: vec![AgentMessageContent::Text {
                text: "queued original".into(),
            }],
        },
    ])
}
fn decision(value: StepInputDecision) -> SessionFact {
    SessionFact::new(
        4,
        4,
        SessionFactBody::StepInputsPrepared {
            turn_id: TurnId::new("input-turn").unwrap(),
            step_id: StepId::new("step").unwrap(),
            decision: value,
        },
    )
    .unwrap()
}
fn rewritten(text: Option<&str>) -> PreparedInputMessage {
    PreparedInputMessage::new(
        PreparedInputRole::User,
        MessageAttribution {
            id: "original".into(),
            source: json!({"kind":"user"}),
        },
        text.into_iter()
            .map(|text| AgentMessageContent::Text { text: text.into() })
            .collect(),
    )
    .unwrap()
}
fn cursor() -> ContextFold {
    ContextFold::with_limits(
        header(""),
        ContextLimits::default(),
        rsi_agent_context::ContextBudget::default(),
    )
    .unwrap()
}

#[test]
fn runtime_snapshot_advances_only_when_entered_and_survives_checkpoint() {
    for enter in [false, true] {
        let mut fold = cursor();
        let mut history = initial();
        let mut body = history.pop().unwrap().body().clone();
        let SessionFactBody::InputMessageClaimed {
            source, content, ..
        } = &mut body
        else {
            panic!()
        };
        *source = InputMessageSource::RuntimeContext;
        *content = vec![AgentMessageContent::Text {
            text: "Directory: /project".into(),
        }];
        history.push(SessionFact::new(3, 3, body).unwrap());
        fold.apply(&history).unwrap();
        assert_eq!(
            fold.runtime_context(),
            None,
            "an unprepared claim is not entered"
        );
        fold.apply(&[decision(if enter {
            StepInputDecision::enter(vec![PreparedInput::Claimed {
                claim: InputClaimRef::new(3, 0).unwrap(),
            }])
            .unwrap()
        } else {
            StepInputDecision::reject()
        })])
        .unwrap();
        let restored = ContextFold::from_checkpoint(
            header(""),
            ContextLimits::default(),
            &fold.checkpoint_bytes().unwrap(),
            rsi_agent_context::ContextBudget::default(),
        )
        .unwrap();
        for fold in [&fold, &restored] {
            assert_eq!(
                fold.runtime_context(),
                enter.then_some("Directory: /project")
            );
            let context = fold.project(ContextLimits::default()).unwrap();
            if enter {
                assert_eq!(context.messages()[0].role(), MessageRole::User);
            } else {
                assert!(context.messages().is_empty());
            }
        }
    }
}

#[test]
fn file_identity_and_execution_location_survive_preparation_and_checkpoint() {
    let file = rsi_media_protocol::FileRef::new(
        rsi_media_protocol::MediaId::new("c".repeat(64)).unwrap(),
        "dataset.csv",
        8_000_000_000,
    )
    .unwrap();
    for local_execution in [true, false] {
        let mut value = serde_json::to_value(header("")).unwrap();
        if !local_execution {
            value["coordinates"]["location"] = json!({"kind":"ssh","target":"e".repeat(32)});
        }
        let header: SessionHeader = serde_json::from_value(value).unwrap();
        let mut history = initial();
        let mut body = history.pop().unwrap().body().clone();
        let SessionFactBody::InputMessageClaimed { content, .. } = &mut body else {
            panic!()
        };
        *content = vec![AgentMessageContent::File { file: file.clone() }];
        history.push(SessionFact::new(3, 3, body).unwrap());
        history.push(decision(
            StepInputDecision::enter(vec![
                PreparedInput::Claimed {
                    claim: InputClaimRef::new(1, 0).unwrap(),
                },
                PreparedInput::Claimed {
                    claim: InputClaimRef::new(3, 0).unwrap(),
                },
            ])
            .unwrap(),
        ));
        let mut fold = ContextFold::with_limits(
            header.clone(),
            ContextLimits::default(),
            rsi_agent_context::ContextBudget::default(),
        )
        .unwrap();
        fold.apply(&history).unwrap();
        let checkpoint = fold.checkpoint_bytes().unwrap();
        let restored = ContextFold::from_checkpoint(
            header,
            ContextLimits::default(),
            &checkpoint,
            rsi_agent_context::ContextBudget::default(),
        )
        .unwrap();
        for cursor in [&fold, &restored] {
            let request = cursor.project(ContextLimits::default()).unwrap();
            assert_eq!(
                request.messages()[1].content(),
                &[MessageContent::File {
                    file: file.clone(),
                    local_execution,
                }]
            );
            assert!(serde_json::to_vec(request.messages()).unwrap().len() < 2048);
        }
    }
}

#[test]
fn original_claims_wait_for_preparation_and_checkpoint_preserves_that_boundary() {
    let mut fold = cursor();
    fold.apply(&initial()).unwrap();
    assert!(
        fold.project(ContextLimits::default())
            .unwrap()
            .messages()
            .is_empty()
    );
    assert_eq!(
        fold.claimed_inputs(&TurnId::new("input-turn").unwrap())
            .unwrap()
            .len(),
        2
    );
    let checkpoint = fold.checkpoint_bytes().unwrap();
    let mut restored = ContextFold::from_checkpoint(
        header(""),
        ContextLimits::default(),
        &checkpoint,
        rsi_agent_context::ContextBudget::default(),
    )
    .unwrap();
    let entered = decision(
        StepInputDecision::enter(vec![
            PreparedInput::Replacement {
                claim: InputClaimRef::new(1, 0).unwrap(),
                message: rewritten(Some("rewritten final")),
            },
            PreparedInput::Replacement {
                claim: InputClaimRef::new(3, 0).unwrap(),
                message: rewritten(None),
            },
        ])
        .unwrap(),
    );
    fold.apply(std::slice::from_ref(&entered)).unwrap();
    restored.apply(&[entered]).unwrap();
    let output = fold.project(ContextLimits::default()).unwrap();
    assert_eq!(output.messages().len(), 1);
    assert_eq!(
        output.messages()[0].content(),
        [MessageContent::Text {
            text: "rewritten final".into()
        }]
    );
    assert_eq!(
        output.messages(),
        restored
            .project(ContextLimits::default())
            .unwrap()
            .messages()
    );
    assert!(
        fold.claimed_inputs(&TurnId::new("input-turn").unwrap())
            .unwrap()
            .is_empty()
    );
}

#[test]
fn reject_and_empty_entry_both_consume_claims_without_entering_original_text() {
    for final_input in [
        StepInputDecision::reject(),
        StepInputDecision::enter(vec![]).unwrap(),
    ] {
        let mut fold = cursor();
        fold.apply(&initial()).unwrap();
        fold.apply(&[decision(final_input)]).unwrap();
        assert!(
            fold.project(ContextLimits::default())
                .unwrap()
                .messages()
                .is_empty()
        );
        assert!(
            fold.claimed_inputs(&TurnId::new("input-turn").unwrap())
                .unwrap()
                .is_empty()
        );
        let checkpoint = fold.checkpoint_bytes().unwrap();
        let restored = ContextFold::from_checkpoint(
            header(""),
            ContextLimits::default(),
            &checkpoint,
            rsi_agent_context::ContextBudget::default(),
        )
        .unwrap();
        assert!(
            restored
                .project(ContextLimits::default())
                .unwrap()
                .messages()
                .is_empty()
        );
    }
}

#[test]
fn claim_coordinates_cannot_reach_outside_the_captured_set_or_be_consumed_again() {
    for claim in [
        InputClaimRef::new(2, 0).unwrap(),
        InputClaimRef::new(1, 1).unwrap(),
        InputClaimRef::new(99, 0).unwrap(),
    ] {
        let mut fold = cursor();
        fold.apply(&initial()).unwrap();
        assert!(
            fold.apply(&[decision(
                StepInputDecision::enter(vec![PreparedInput::Claimed { claim }]).unwrap()
            )])
            .is_err()
        );
    }
    let mut fold = cursor();
    fold.apply(&initial()).unwrap();
    let selected = StepInputDecision::enter(vec![PreparedInput::Claimed {
        claim: InputClaimRef::new(3, 0).unwrap(),
    }])
    .unwrap();
    fold.apply(&[decision(selected.clone())]).unwrap();
    let repeated = SessionFact::new(
        5,
        5,
        SessionFactBody::StepInputsPrepared {
            turn_id: TurnId::new("input-turn").unwrap(),
            step_id: StepId::new("step").unwrap(),
            decision: selected,
        },
    )
    .unwrap();
    assert!(fold.apply(&[repeated]).is_err());
}

#[test]
fn reordered_and_repeated_output_uses_one_claim_consumption() {
    let mut fold = cursor();
    fold.apply(&initial()).unwrap();
    fold.apply(&[decision(
        StepInputDecision::enter(
            [3, 1, 3]
                .into_iter()
                .map(|seq| PreparedInput::Claimed {
                    claim: InputClaimRef::new(seq, 0).unwrap(),
                })
                .collect(),
        )
        .unwrap(),
    )])
    .unwrap();
    let projection = fold.project(ContextLimits::default()).unwrap();
    let text = projection
        .messages()
        .iter()
        .map(rsi_ai_protocol::Message::content)
        .collect::<Vec<_>>();
    assert_eq!(text.len(), 3);
    assert_eq!(text[0], text[2]);
    assert_ne!(text[0], text[1]);
    assert!(
        fold.claimed_inputs(&TurnId::new("input-turn").unwrap())
            .unwrap()
            .is_empty()
    );
}
