use rsi_agent_session_protocol::{
    AgentMessageContent, InputMessageSource, MessageId, SessionFact, SessionFactBody, StepId,
    TurnId,
};
use rsi_conversation::{
    EnteredContent, EnteredInput, FactField, FieldValue, InputPart, select_field,
};
use rsi_media_protocol::{FileRef, MediaId};

#[test]
fn running_and_deleting_a_schedule_use_their_distinct_transcript_roles() {
    use rsi_agent_session_protocol::{CommandMessageSource, ContributionId, DomainRequestId};
    for (name, role, title) in [
        (
            "rsi.schedule.run",
            "trigger-schedule",
            Some("Automation task"),
        ),
        (
            "rsi.schedule.delete",
            "step-context",
            Some("Automation task"),
        ),
        ("example.command", "user", None),
    ] {
        let source = CommandMessageSource {
            command: ContributionId::new(name).unwrap(),
            request_id: DomainRequestId::new("task").unwrap(),
            invocation_sha256: "a".repeat(64),
            ordinal: 0,
            attribution: None,
        };
        let claim = InputMessageSource::Command {
            message_id: source.message_id().unwrap(),
            source,
        };
        let input = EnteredInput {
            index: 0,
            seq: 1,
            offset: 0,
            source: rsi_conversation::EnteredSource::Claim(&claim),
            content: EnteredContent::Direct("[SCHEDULE REMINDER]"),
        };
        let (actual_role, actual_title) = input.presentation();
        assert_eq!((actual_role, actual_title.as_deref()), (role, title));
        assert!(!input.is_human());
    }
}

#[test]
fn runtime_context_label_follows_provenance_instead_of_message_text() {
    let human = InputMessageSource::Human {
        message_id: MessageId::new("human").unwrap(),
        attribution: None,
    };
    let runtime = InputMessageSource::RuntimeContext;
    let input = EnteredInput {
        index: 0,
        source: rsi_conversation::EnteredSource::Claim(&human),
        seq: 1,
        offset: 0,
        content: EnteredContent::Direct("Runtime context"),
    };
    assert_eq!(input.presentation(), ("user", None));
    assert!(input.is_human());
    assert_eq!(
        EnteredInput {
            source: rsi_conversation::EnteredSource::Claim(&runtime),
            ..input
        }
        .presentation()
        .1
        .as_deref(),
        Some("Runtime context")
    );
}

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
        source: rsi_conversation::EnteredSource::Claim(source),
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
