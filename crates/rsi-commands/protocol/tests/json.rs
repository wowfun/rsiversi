use rsi_commands_protocol::{
    CommandDescriptor, CommandRequest, MAXIMUM_COMMAND_NAME_BYTES, MAXIMUM_COMMAND_TEXT_BYTES,
};
use serde_json::json;

#[test]
fn valid_command_json_round_trips_at_utf8_byte_boundaries() {
    let text = "🌱".repeat(MAXIMUM_COMMAND_TEXT_BYTES / 4);
    let request = CommandRequest {
        name: "命".repeat(MAXIMUM_COMMAND_NAME_BYTES / 3) + "x",
        text,
    };
    request.validate().unwrap();
    assert_eq!(request.name.len(), MAXIMUM_COMMAND_NAME_BYTES);
    assert_eq!(
        serde_json::from_slice::<CommandRequest>(&serde_json::to_vec(&request).unwrap()).unwrap(),
        request
    );
    let descriptor = CommandDescriptor {
        name: request.name.clone(),
        description: request.text.clone(),
    };
    assert_eq!(
        serde_json::from_slice::<CommandDescriptor>(&serde_json::to_vec(&descriptor).unwrap())
            .unwrap(),
        descriptor
    );
    let mut wire = serde_json::to_value(&request).unwrap();
    wire["text"] = json!(request.text + "x");
    assert!(serde_json::from_value::<CommandRequest>(wire).is_err());
}

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
