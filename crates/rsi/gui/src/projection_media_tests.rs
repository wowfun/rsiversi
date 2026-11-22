use super::*;
use rsi_agent_session_protocol::{
    AgentMessageContent, EffectId, InputMessageSource, MessageId, StepId,
};

fn input_with_image(media: &rsi_media_protocol::MediaRef) -> SessionFact {
    SessionFact::new(
        1,
        1,
        SessionFactBody::InputMessageClaimed {
            turn_id: TurnId::new("turn").unwrap(),
            step_id: StepId::new("step").unwrap(),
            source: InputMessageSource::Human {
                attribution: None,
                message_id: MessageId::new("input").unwrap(),
            },
            content: vec![
                AgentMessageContent::Text {
                    text: "before".into(),
                },
                AgentMessageContent::Image {
                    presentation: Some(Box::new(
                        serde_json::from_value(serde_json::json!({"name":"swatch.jpeg"})).unwrap(),
                    )),
                    media: media.clone(),
                },
                AgentMessageContent::Text {
                    text: "after".into(),
                },
                AgentMessageContent::Image {
                    media: media.clone(),
                    presentation: None,
                },
            ],
        },
    )
    .unwrap()
}

#[test]
fn input_tool_and_generated_images_keep_distinct_exact_provenance() {
    let image = SessionFact::new(3, 1, SessionFactBody::ImageOutput {
        turn_id: TurnId::new("turn").unwrap(), effect_id: EffectId::new("effect").unwrap(), index: 0,
        media: serde_json::from_value(serde_json::json!({"id":"d".repeat(64), "mime":"image/png", "bytes":7, "width":1, "height":1})).unwrap(),
    }).unwrap();
    let SessionFactBody::ImageOutput { media, .. } = image.body() else {
        unreachable!()
    };
    let input = input_with_image(media);
    let tool = SessionFact::new(
        2,
        1,
        SessionFactBody::ToolResult {
            turn_id: TurnId::new("turn").unwrap(),
            effect_id: EffectId::new("effect").unwrap(),
            identity: rsi_tools_protocol::ToolResultIdentity::new(
                "owner",
                "invocation",
                "call",
                "a".repeat(64),
            )
            .unwrap(),
            result: rsi_tools_protocol::ToolResult::new(
                serde_json::json!({}),
                vec![
                    ToolContent::Text {
                        text: "before".into(),
                    },
                    ToolContent::Image {
                        media: media.clone(),
                    },
                    ToolContent::Text {
                        text: "after".into(),
                    },
                ],
                false,
            )
            .unwrap(),
            conclusion: None,
        },
    )
    .unwrap();
    let mut transcript = Transcript::default();
    for fact in [&image, &tool, &input, &input, &tool, &image] {
        if let SessionFactBody::InputMessageClaimed {
            source, content, ..
        } = fact.body()
        {
            transcript.entered(
                fact,
                rsi_conversation::EnteredInput {
                    index: 0,
                    seq: fact.seq(),
                    offset: 0,
                    source: Some(source),
                    content: rsi_conversation::EnteredContent::Blocks(content),
                },
            );
        }
        transcript.fact(fact);
    }
    assert_eq!(transcript.blocks.len(), 3);
    let message = &transcript.blocks[0];
    assert_eq!(message.text, "before\nafter");
    assert_eq!(message.sources.len(), 4);
    assert!(
        message
            .sources
            .position(SourceRef {
                seq: 1,
                field: FactField::InputImage { index: 1 }
            })
            .is_some()
    );
    assert_eq!(
        serde_json::to_value(message).unwrap()["images"],
        serde_json::json!([{"media":media, "name":"swatch.jpeg"}, {"media":media}])
    );
    assert_eq!(
        transcript.blocks[1].text,
        "before[Image · image/png · 1×1 · 7 bytes]after"
    );
    assert_eq!(transcript.blocks[1].sources.len(), 4);
    assert!(
        transcript.blocks[1]
            .sources
            .position(SourceRef {
                seq: 2,
                field: FactField::ToolImage { index: 1 }
            })
            .is_some()
    );
    assert_eq!(
        transcript.blocks[2].sources.get(0).unwrap().field,
        FactField::ImageOutput
    );
}
