use super::*;
use rsi_agent_session_protocol::{EffectId, MessageId, StepId, TurnId};

#[test]
fn ordered_images_preserve_exact_sources_and_selection_mapping() {
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
                vec![ToolContent::Image {
                    media: media.clone(),
                }],
                false,
            )
            .unwrap(),
        },
    )
    .unwrap();
    let mut transcript = Transcript::default();
    for fact in [&image, &tool, &input, &input, &tool, &image] {
        transcript.apply(fact);
    }
    assert_eq!(transcript.blocks.len(), 3);
    let block = &transcript.blocks[0];
    assert_eq!(block.pieces.len(), 3);
    assert_eq!(
        block.pieces[1].source.field,
        FactField::InputImage { index: 1 }
    );
    assert!(block.text().starts_with("before{"));
    assert!(block.text().ends_with("}after"));
    let piece = &block.pieces[1];
    assert_eq!(
        piece.text,
        rsi_conversation::select_field(&input, piece.source)
            .unwrap()
            .window(0, WINDOW)
            .unwrap()
            .text
    );
    assert_eq!(piece.anchor(piece.text.len()).offset, piece.text.len());
    assert_eq!(
        transcript.blocks[1].pieces[0].source.field,
        FactField::ToolImage { index: 0 }
    );
    assert_eq!(
        transcript.blocks[2].pieces[0].source.field,
        FactField::ImageOutput
    );
}
