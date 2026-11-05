use rsi_agent_composition_protocol::{CommandInput, CommandInputDelivery, PreparedCommandInput};
use rsi_agent_session_protocol::{
    AgentMessageContent, CommandArguments, CommandRevision, ContributionId, DomainRequestId,
    MessageDelivery, SessionCommandInvocation,
};

#[test]
fn publication_moves_callback_content_and_counts_the_exact_escaped_wire_bytes() {
    let invocation = SessionCommandInvocation {
        command: ContributionId::new("test.input").unwrap(),
        request_id: DomainRequestId::new("test-message").unwrap(),
        expected_revision: CommandRevision::Draft { revision: 0 },
        arguments: CommandArguments::new(serde_json::Value::Null).unwrap(),
    };
    let text = "quote \" and newline\n".repeat(500);
    let original = text.as_ptr();
    let inputs = vec![CommandInput {
        delivery: CommandInputDelivery::Steer,
        content: vec![AgentMessageContent::Text { text }],
        attribution: None,
    }];
    let mut batch = PreparedCommandInput::prepare(&invocation, inputs).unwrap();
    let input = batch.pop().unwrap();
    let held = input.publication(true);
    let encoded = serde_json::to_vec(&held).unwrap().len();
    assert_eq!(held.encoded_len().unwrap(), encoded);
    let maximum = (rsi_agent_session_protocol::MAXIMUM_COMMAND_INPUT_BYTES - 1) / (encoded + 1);
    assert!(maximum < rsi_agent_session_protocol::MAXIMUM_COMMAND_INPUTS);
    let repeated = vec![input.clone(); maximum + 1];
    PreparedCommandInput::validate_batch(&repeated[..maximum]).unwrap();
    assert!(PreparedCommandInput::validate_batch(&repeated).is_err());
    let message = input.into_publication(false);
    let AgentMessageContent::Text { text } = &message.message.content[0] else {
        panic!("expected callback text");
    };
    assert_eq!(
        text.as_ptr(),
        original,
        "durable command copied the text allocation"
    );
    assert_eq!(message.delivery, MessageDelivery::Steer);
}
