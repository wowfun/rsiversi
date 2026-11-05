use rsi_agent_session_protocol::*;
use serde_json::json;

fn input() -> AgentMessage {
    AgentMessage {
        message_id: MessageId::new("native-identity").unwrap(),
        source: AgentMessageSource::Human {
            attribution: Some(MessageAttribution {
                id: "原始 ID:可以重复使用".into(),
                source: json!({"kind":"plugin:followup","meta":[true,4,"值"]}),
            }),
        },
        content: vec![
            AgentMessageContent::Text {
                text: "first".into(),
            },
            AgentMessageContent::Text {
                text: "第二块".into(),
            },
        ],
        options: MessageOptions::default(),
    }
}

#[test]
fn direct_ingress_keeps_native_identity_original_attribution_and_delivery() {
    let message = input();
    message.validate().unwrap();
    assert_eq!(message.source.kind(), AgentMessageSourceKind::Human);
    let source = message.claimed_source();
    let InputMessageSource::Human {
        message_id,
        attribution,
    } = &source
    else {
        panic!("direct ingress must retain its origin");
    };
    assert_eq!(message_id, &message.message_id);
    assert_eq!(attribution.as_ref().unwrap().id, "原始 ID:可以重复使用");
    let fact = SessionFact::new(
        1,
        1,
        SessionFactBody::InputMessageClaimed {
            turn_id: TurnId::new("turn").unwrap(),
            step_id: StepId::new("step").unwrap(),
            source,
            content: message.content.clone(),
        },
    )
    .unwrap();
    assert_eq!(
        serde_json::from_value::<SessionFact>(serde_json::to_value(&fact).unwrap()).unwrap(),
        fact
    );
    for (delivery, target, bound) in [
        (MessageDelivery::NextTurn, MessageTarget::NextTurn, None),
        (MessageDelivery::Steer, MessageTarget::NextTurn, None),
        (
            MessageDelivery::Steer,
            MessageTarget::NextStep,
            Some(TurnId::new("active").unwrap()),
        ),
    ] {
        delivery
            .validate_route(&message, target, bound.as_ref())
            .unwrap();
    }
}

#[test]
fn attribution_is_bounded_on_decode_and_at_each_durable_input_boundary() {
    let message = input();
    for invalid in [
        json!({"id":"", "source":null}),
        json!({"id":"界".repeat(86), "source":null}),
        json!({"id":"nul\0byte", "source":null}),
        json!({"id":"safe", "source":{"data":"x".repeat(16 * 1024)}}),
        json!({"id":"safe", "source":null, "authority":true}),
    ] {
        assert!(serde_json::from_value::<MessageAttribution>(invalid.clone()).is_err());
        let mut wire = serde_json::to_value(&message).unwrap();
        wire["source"]["attribution"] = invalid;
        assert!(serde_json::from_value::<AgentMessage>(wire).is_err());
    }
    let mut invalid = message;
    let AgentMessageSource::Human {
        attribution: Some(attribution),
    } = &mut invalid.source
    else {
        unreachable!()
    };
    attribution.id.clear();
    assert!(invalid.validate().is_err());
    assert!(
        SessionFact::new(
            1,
            1,
            SessionFactBody::InputMessageClaimed {
                turn_id: TurnId::new("turn").unwrap(),
                step_id: StepId::new("step").unwrap(),
                source: invalid.claimed_source(),
                content: invalid.content,
            }
        )
        .is_err()
    );
}
