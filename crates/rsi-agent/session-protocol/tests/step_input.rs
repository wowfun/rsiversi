use rsi_agent_session_protocol::*;
use serde_json::json;

#[test]
fn original_mods_decisions_decode_without_losing_the_emptied_second_message() {
    #[derive(serde::Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Fixture {
        rewrite: StepInputDecision,
        reject: StepInputDecision,
    }
    let fixture: Fixture =
        serde_json::from_str(include_str!("fixtures/prepared-input.json")).unwrap();
    let [
        PreparedInput::Replacement {
            claim: first,
            message,
        },
        PreparedInput::Replacement {
            claim: second,
            message: empty,
        },
    ] = fixture.rewrite.messages().unwrap()
    else {
        panic!("original Mods retains both rewritten message identities");
    };
    assert_eq!(first.fact_seq(20), 4);
    assert_eq!(message.attribution().id, "message-0");
    assert_eq!(
        message.content(),
        [AgentMessageContent::Text {
            text: "rewritten by original mod".into(),
        }]
    );
    assert_eq!(second.fact_seq(20), 5);
    assert_eq!(empty.attribution().id, "message-1");
    assert!(empty.content().is_empty());
    assert!(fixture.reject.messages().is_none());
}

fn message(id: &str, blocks: &[&str]) -> PreparedInputMessage {
    PreparedInputMessage::new(
        PreparedInputRole::User,
        MessageAttribution {
            id: id.into(),
            source: json!({"kind":"user", "origin":"原始消息"}),
        },
        blocks
            .iter()
            .map(|text| AgentMessageContent::Text {
                text: (*text).into(),
            })
            .collect(),
    )
    .unwrap()
}

#[test]
fn final_input_preserves_reordering_replacement_additions_and_repeated_presentation() {
    let first = InputClaimRef::new(4, 0).unwrap();
    let later = InputClaimRef::new(9, 12).unwrap();
    let rewritten = message("original-id", &["rewritten", "attached context"]);
    let decision = StepInputDecision::enter(vec![
        PreparedInput::Claimed { claim: later },
        PreparedInput::Replacement {
            claim: first,
            message: rewritten.clone(),
        },
        PreparedInput::Claimed { claim: later },
        PreparedInput::Added {
            message: message("added", &["new context"]),
        },
    ])
    .unwrap();
    let bytes = serde_json::to_vec(&decision).unwrap();
    let replay: StepInputDecision = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(replay, decision);
    let entries = replay.messages().unwrap();
    assert_eq!(entries[0], entries[2]);
    assert!(
        matches!(&entries[1], PreparedInput::Replacement { claim, message }
        if *claim == first && message == &rewritten)
    );
    assert_eq!(rewritten.attribution().id, "original-id");
    assert_eq!(rewritten.role(), PreparedInputRole::User);
    assert_eq!(later.fact_seq(20), 9);
    assert_eq!(later.ordinal(), 12);
}

#[test]
fn reject_empty_enter_and_empty_message_are_distinct() {
    let rejected = StepInputDecision::reject();
    let empty = StepInputDecision::enter(vec![]).unwrap();
    let empty_message = StepInputDecision::enter(vec![PreparedInput::Replacement {
        claim: InputClaimRef::new(3, 0).unwrap(),
        message: message("human", &[]),
    }])
    .unwrap();
    for decision in [&rejected, &empty, &empty_message] {
        let replay: StepInputDecision =
            serde_json::from_str(&serde_json::to_string(decision).unwrap()).unwrap();
        assert_eq!(&replay, decision);
    }
    assert!(rejected.messages().is_none());
    assert_eq!(empty.messages(), Some([].as_slice()));
    assert_eq!(empty_message.messages().unwrap().len(), 1);
    assert_eq!(message("cleared", &[""]).content().len(), 1);
    // The original ingress boundary remains strict; rewrite output is a different operation.
    let input = AgentMessage {
        message_id: MessageId::new("empty-ingress").unwrap(),
        source: AgentMessageSource::Human { attribution: None },
        content: vec![],
        options: MessageOptions::default(),
    };
    assert!(input.validate().is_err());
}

#[test]
fn unchanged_input_serializes_only_its_coordinate() {
    let decision = StepInputDecision::enter(vec![PreparedInput::Claimed {
        claim: InputClaimRef::new(u64::MAX, u32::MAX).unwrap(),
    }])
    .unwrap();
    assert_eq!(
        serde_json::to_value(&decision).unwrap(),
        json!({"kind":"enter","messages":[{"kind":"claimed",
            "claim":{"fact_seq":u64::MAX,"ordinal":u32::MAX}}]})
    );
    assert!(serde_json::to_vec(&decision).unwrap().len() < 128);
}

#[test]
fn external_decisions_cannot_bypass_typed_input_invariants() {
    assert!(InputClaimRef::new(0, 0).is_err());
    let valid = serde_json::to_value(message("message", &["text"])).unwrap();
    for invalid in [
        json!({"kind":"reject","messages":[]}),
        json!({"kind":"enter","messages":[{"kind":"claimed","claim":{"fact_seq":0,"ordinal":0}}]}),
        json!({"kind":"enter","messages":[{"kind":"added","message":{
            "role":"system","attribution":{"id":"forged","source":{}},"content":[]}}]}),
        json!({"kind":"enter","messages":[{"kind":"replacement","message":valid}]}),
    ] {
        assert!(
            serde_json::from_value::<StepInputDecision>(invalid.clone()).is_err(),
            "unexpectedly admitted {invalid}"
        );
    }
    for (field, invalid) in [
        ("content", json!([{"type":"text","text":"nul\0byte"}])),
        ("attribution", json!({"id":"","source":{}})),
        (
            "content",
            json!([{"type":"text","text":"x".repeat(MAXIMUM_TURN_TEXT_BYTES+1)}]),
        ),
        (
            "content",
            json!(vec![
                json!({"type":"text","text":"x"});
                MAXIMUM_AGENT_MESSAGE_CONTENT_BLOCKS + 1
            ]),
        ),
    ] {
        let mut wire = valid.clone();
        wire[field] = invalid;
        assert!(serde_json::from_value::<PreparedInputMessage>(wire).is_err());
    }
}

#[test]
fn final_list_obeys_its_encoded_budget_without_a_second_message_count_cap() {
    let large = message("large", &[&"x".repeat(MAXIMUM_TURN_TEXT_BYTES)]);
    let entries = (0..31)
        .map(|_| PreparedInput::Added {
            message: large.clone(),
        })
        .collect();
    let below = StepInputDecision::enter(entries).unwrap();
    assert_eq!(below.messages().unwrap().len(), 31);
    drop(below);
    let entries = (0..32)
        .map(|_| PreparedInput::Added {
            message: large.clone(),
        })
        .collect();
    assert!(StepInputDecision::enter(entries).is_err());
    // More than a pending tool batch can enter as compact references. Context owns
    // the eventual provider history limit; this decoder adds no separate count gate.
    let many = StepInputDecision::enter(vec![
        PreparedInput::Claimed {
            claim: InputClaimRef::new(1, 0).unwrap(),
        };
        MAXIMUM_TOOL_CONTEXT_MESSAGES
            + MAXIMUM_PENDING_AGENT_MESSAGES
            + 1
    ])
    .unwrap();
    assert_eq!(many.messages().unwrap().len(), 4161);
}

#[test]
fn relative_claim_survives_concurrent_prefix_growth_and_decision_size_matches_wire() {
    let claim = InputClaimRef::preceding(2).unwrap();
    assert_eq!(claim.fact_seq(5), 3);
    assert_eq!(claim.fact_seq(9), 7);
    assert_eq!(claim.fact_seq(1), 0);
    assert!(
        serde_json::from_value::<InputClaimRef>(serde_json::json!({"preceding":0,"ordinal":0}))
            .is_err()
    );
    for decision in [
        StepInputDecision::reject(),
        StepInputDecision::enter(vec![PreparedInput::Replacement {
            claim,
            message: message("sized", &["界🚀", "second"]),
        }])
        .unwrap(),
    ] {
        let bytes = serde_json::to_vec(&decision).unwrap();
        assert_eq!(bytes.len(), decision.encoded_len());
        assert_eq!(
            serde_json::from_slice::<StepInputDecision>(&bytes).unwrap(),
            decision
        );
    }
}
