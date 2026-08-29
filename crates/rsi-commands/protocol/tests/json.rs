use rsi_commands_protocol::{
    CommandDescriptor, CommandRequest, MAXIMUM_COMMAND_NAME_BYTES, MAXIMUM_COMMAND_TEXT_BYTES,
};
use serde_json::json;

#[test]
fn command_deserialization_revalidates_request_and_descriptor_bounds() {
    assert!(
        serde_json::from_value::<CommandRequest>(json!({
            "name": "x".repeat(MAXIMUM_COMMAND_NAME_BYTES + 1),
            "text": "arguments"
        }))
        .is_err()
    );
    assert!(
        serde_json::from_value::<CommandDescriptor>(json!({
            "name": "command",
            "description": "x".repeat(MAXIMUM_COMMAND_TEXT_BYTES + 1)
        }))
        .is_err()
    );
}
