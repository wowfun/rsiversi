use super::*;
use rsi_agent_session_protocol::{
    AgentMessageContent, EffectId, InputMessageSource, MessageId, StepId, TurnId,
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
                    presentation: None,
                    media: media.clone(),
                },
                AgentMessageContent::Text {
                    text: "after".into(),
                },
            ],
        },
    )
    .unwrap()
}

#[test]
fn entered_context_folds_once_and_keeps_exact_text_after_expansion_and_replay() {
    let source = serde_json::json!({"kind":"skill-catalog"});
    let text = "<system-reminder>Available skills</system-reminder>";
    let content = [AgentMessageContent::Text { text: text.into() }];
    let row = rsi_conversation::EnteredInput {
        index: 0,
        seq: 7,
        offset: 0,
        source: rsi_conversation::EnteredSource::Application(&source),
        content: rsi_conversation::EnteredContent::Blocks(&content),
    };
    let mut transcript = Transcript::default();
    transcript.entered(7, 1, row, false);
    let block = &mut transcript.blocks[0];
    assert_eq!(block.title, "skill-catalog");
    assert_eq!(block.role, Role::Status);
    assert!(block.summary_only());
    block.collapsed = false;
    transcript.entered(7, 1, row, true);
    assert_eq!(transcript.blocks.len(), 1);
    assert!(!transcript.blocks[0].summary_only());
    assert_eq!(transcript.blocks[0].text(), text);
    let long_kind = serde_json::json!({"kind":"\u{001b}界".repeat(1800)});
    transcript.entered(
        8,
        1,
        rsi_conversation::EnteredInput {
            index: 1,
            seq: 8,
            source: rsi_conversation::EnteredSource::Application(&long_kind),
            ..row
        },
        false,
    );
    let restored = Viewport::capture(&transcript, None, 80, 24)
        .0
        .restore()
        .unwrap();
    assert!(restored.blocks.last().unwrap().summary_only());
}

#[test]
fn ordered_images_preserve_exact_sources_and_selection_mapping() {
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
                vec![ToolContent::Image {
                    media: media.clone(),
                }],
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
                fact.seq(),
                fact.timestamp_ms(),
                rsi_conversation::EnteredInput {
                    index: 0,
                    seq: fact.seq(),
                    offset: 0,
                    source: rsi_conversation::EnteredSource::Claim(source),
                    content: rsi_conversation::EnteredContent::Blocks(content),
                },
                false,
            );
        }
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
