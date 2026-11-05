use rsi_agent_session_protocol::{
    AgentMessageContent, InputMessageSource, MessageId, SessionFact, SessionFactBody, StepId,
    TurnId,
};
use rsi_conversation::{
    EnteredContent, EnteredInput, FactField, FieldValue, InputPart, select_field,
};
use rsi_media_protocol::{FileRef, MediaId};

#[test]
fn file_input_has_an_exact_detail_source_without_loading_or_storing_paths() {
    let file = FileRef::new(MediaId::new("a".repeat(64)).unwrap(), "report.csv", 123).unwrap();
    let fact = SessionFact::new(
        1,
        1,
        SessionFactBody::InputMessageClaimed {
            turn_id: TurnId::new("turn").unwrap(),
            step_id: StepId::new("step").unwrap(),
            source: InputMessageSource::Human {
                message_id: MessageId::new("message").unwrap(),
                attribution: None,
            },
            content: vec![AgentMessageContent::File { file: file.clone() }],
        },
    )
    .unwrap();
    let SessionFactBody::InputMessageClaimed {
        source, content, ..
    } = fact.body()
    else {
        unreachable!()
    };
    let entered = EnteredInput {
        index: 0,
        source: Some(source),
        seq: 1,
        offset: 0,
        content: EnteredContent::Blocks(content),
    };
    let (source, part) = entered.parts().next().unwrap();
    assert_eq!(source.field, FactField::InputFile { index: 0 });
    let InputPart::File(displayed) = part else {
        panic!("lost file identity")
    };
    assert_eq!(displayed, &file);
    let detail = select_field(&fact, source).unwrap();
    assert!(matches!(detail, FieldValue::File(value) if value == &file));
    let encoded = serde_json::to_value(source).unwrap();
    assert_eq!(
        serde_json::from_value::<rsi_conversation::SourceRef>(encoded).unwrap(),
        source
    );
    assert!(
        select_field(
            &fact,
            rsi_conversation::SourceRef {
                seq: 1,
                field: FactField::InputFile { index: 1 }
            }
        )
        .is_none()
    );
}
