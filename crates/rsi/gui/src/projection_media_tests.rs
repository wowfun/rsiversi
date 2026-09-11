use super::*;
use rsi_agent_session_protocol::{EffectId, MessageId, StepId};

#[test]
fn input_tool_and_generated_images_keep_distinct_exact_provenance() {
    let image = SessionFact::new(3, 1, SessionFactBody::ImageOutput {
        turn_id: TurnId::new("turn").unwrap(), effect_id: EffectId::new("effect").unwrap(), index: 0,
        media: serde_json::from_value(serde_json::json!({"id":"d".repeat(64), "mime":"image/png", "bytes":7, "width":1, "height":1})).unwrap(),
    }).unwrap();
    let SessionFactBody::ImageOutput { media, .. } = image.body() else {
        unreachable!()
    };
    let input = SessionFact::new(
        1,
        1,
        SessionFactBody::InputMessageEntered {
            turn_id: TurnId::new("turn").unwrap(),
            step_id: StepId::new("step").unwrap(),
            source: InputMessageSource::Human {
                message_id: MessageId::new("input").unwrap(),
            },
            content: vec![
                AgentMessageContent::Text {
                    text: "before".into(),
                },
                AgentMessageContent::Image {
                    media: media.clone(),
                },
                AgentMessageContent::Text {
                    text: "after".into(),
                },
            ],
        },
    )
    .unwrap();
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
        },
    )
    .unwrap();
    let mut transcript = Transcript::default();
    for fact in [&image, &tool, &input, &input, &tool, &image] {
        transcript.fact(fact);
    }
    assert_eq!(transcript.blocks.len(), 5);
    assert_eq!(transcript.blocks[0].text, "before");
    assert_eq!(
        transcript.blocks[1].text,
        "[Image · image/png · 1×1 · 7 bytes]"
    );
    assert_eq!(transcript.blocks[2].text, "after");
    assert_eq!(
        transcript.blocks[1].sources.get(0).unwrap().field,
        FactField::InputImage { index: 1 }
    );
    assert_eq!(
        transcript.blocks[3].text,
        "before[Image · image/png · 1×1 · 7 bytes]after"
    );
    assert_eq!(transcript.blocks[3].sources.len(), 4);
    assert!(
        transcript.blocks[3]
            .sources
            .position(SourceRef {
                seq: 2,
                field: FactField::ToolImage { index: 1 }
            })
            .is_some()
    );
    assert_eq!(
        transcript.blocks[4].sources.get(0).unwrap().field,
        FactField::ImageOutput
    );
}
