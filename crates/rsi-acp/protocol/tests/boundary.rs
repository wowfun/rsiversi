use rsi_acp_protocol::{
    Error, FrameDecoder, MAX_FRAME_BYTES, decode, validate_permission, validate_prompt,
    validate_session_setup,
};
use serde_json::json;

#[test]
fn malformed_capabilities_and_unsupported_authority_are_not_defaulted() {
    use rsi_acp_protocol::{validate_initialize, validate_list, validate_session_id};
    validate_initialize(&json!({"protocolVersion":1})).unwrap();
    for capabilities in [
        json!(null),
        json!([]),
        json!({"fs":null}),
        json!({"fs":{"readTextFile":"yes"}}),
        json!({"terminal":1}),
        json!({"session":false}),
    ] {
        assert!(
            validate_initialize(&json!({"protocolVersion":1,"clientCapabilities":capabilities}))
                .is_err()
        );
    }
    let cwd = std::env::current_dir().unwrap();
    validate_list(&json!({"cwd":cwd})).unwrap();
    assert!(validate_list(&json!({"cwd":"relative"})).is_err());
    assert!(validate_list(&json!({"cursor":"x".repeat(2049)})).is_err());
    assert!(validate_session_id(&json!({"sessionId":"x".repeat(257)})).is_err());
    for directories in [json!([cwd]), json!(false), json!(null)] {
        assert!(
            validate_session_setup(
                &json!({"cwd":cwd,"mcpServers":[],"additionalDirectories":directories}),
                false
            )
            .is_err()
        );
    }
}

#[test]
fn agent_capability_and_update_schemas_reject_silent_defaults_and_skips() {
    use rsi_acp_protocol::{validate_agent_initialize, validate_session_update};
    let minimal = json!({"protocolVersion":1});
    assert_eq!(
        validate_agent_initialize(&minimal).unwrap(),
        rsi_acp_protocol::observation::Capabilities::default()
    );
    assert!(
        validate_agent_initialize(
            &json!({"protocolVersion":1,"agentCapabilities":{"loadSession":"true"}})
        )
        .is_err()
    );
    assert!(validate_agent_initialize(&json!({"protocolVersion":1,"agentCapabilities":{"sessionCapabilities":{"resume":true}}})).is_err());
    let valid = json!({"sessionId":"remote","update":{"sessionUpdate":"tool_call","toolCallId":"tool","title":"Example","content":[{"type":"content","content":{"type":"text","text":"result"}}]}});
    validate_session_update(&valid).unwrap();
    let mut malformed = valid;
    malformed["update"]["content"]
        .as_array_mut()
        .unwrap()
        .push(json!({"type":"content","content":{"type":"text","text":123}}));
    assert!(validate_session_update(&malformed).is_err());
}

#[test]
fn rejects_ambiguous_envelopes_and_duplicate_nested_authority() {
    for input in [
        r#"{"jsonrpc":"2.0","method":"session/new","params":{"cwd":"/safe","cwd":"/other"}}"#,
        r#"{"jsonrpc":"2.0","id":1,"result":{},"error":{"code":0,"message":"secret"}}"#,
        r#"{"jsonrpc":"2.0","id":1.5,"result":{}}"#,
        r#"{"jsonrpc":"2.0","id":null,"result":{}}"#,
        r#"[{"jsonrpc":"2.0","method":"initialize"}]"#,
        r#"{"jsonrpc":"2.0","method":"initialize","params":[]}"#,
    ] {
        assert!(decode(input.as_bytes()).is_err(), "{input}");
    }
    assert!(decode(br#"{"jsonrpc":"2.0","id":"exact","result":{"fraction":1.23,"huge":18446744073709551617}}"#).is_ok());
}

#[test]
fn fragmented_frames_and_limits_do_not_accept_truncated_eof() {
    let wire = b"{\"jsonrpc\":\"2.0\",\"method\":\"initialize\"}\n";
    for split in 0..wire.len() {
        let mut decoder = FrameDecoder::default();
        assert!(decoder.push(&wire[..split]).unwrap().is_empty());
        let records = decoder.push(&wire[split..]).unwrap();
        assert_eq!(records, [wire[..wire.len() - 1].to_vec()]);
        assert!(decoder.finish().is_ok());
    }
    let mut decoder = FrameDecoder::default();
    decoder.push(b"{}").unwrap();
    assert_eq!(decoder.finish(), Err(Error::Frame));
    let mut decoder = FrameDecoder::default();
    for _ in 0..MAX_FRAME_BYTES / 65536 {
        decoder.push(&vec![b'a'; 65536]).unwrap();
    }
    assert_eq!(decoder.push(b"a"), Err(Error::Limit));
}

#[test]
fn malformed_mcp_cannot_be_silently_skipped_by_schema() {
    let valid = json!({"cwd":std::env::current_dir().unwrap(),"mcpServers":[{"name":"server","command":std::env::current_exe().unwrap(),"args":[],"env":[{"name":"TOKEN","value":"private"}]}]});
    validate_session_setup(&valid, false).unwrap();
    let mut malformed = valid.clone();
    malformed["mcpServers"]
        .as_array_mut()
        .unwrap()
        .push(json!({"name":"broken"}));
    // Upstream deliberately skips this item; our owning boundary must reject it.
    let permissive: rsi_acp_protocol::schema::NewSessionRequest =
        serde_json::from_value(malformed.clone()).unwrap();
    assert_eq!(permissive.mcp_servers.len(), 1);
    assert_eq!(
        validate_session_setup(&malformed, false),
        Err(Error::Parameters)
    );
    for replacement in [
        json!(null),
        json!({}),
        json!([{"type":"http","name":"remote","url":"https://example.invalid"}]),
        json!([
            valid["mcpServers"][0].clone(),
            valid["mcpServers"][0].clone()
        ]),
    ] {
        let mut malformed = valid.clone();
        malformed["mcpServers"] = replacement;
        assert!(validate_session_setup(&malformed, false).is_err());
    }
    let mut duplicate_env = valid;
    duplicate_env["mcpServers"][0]["env"]
        .as_array_mut()
        .unwrap()
        .push(json!({"name":"TOKEN","value":"other"}));
    assert!(validate_session_setup(&duplicate_env, false).is_err());
}

#[test]
fn permission_ids_remain_exact_and_unsupported_prompt_blocks_fail() {
    let mut request = json!({"sessionId":"s","toolCall":{"toolCallId":"t"},"options":[
        {"optionId":"peer-1","name":"Once","kind":"allow_once"},
        {"optionId":"peer-2","name":"Always","kind":"allow_always"},
        {"optionId":"peer-3","name":"Reject","kind":"reject_once"},
        {"optionId":"peer-4","name":"Never","kind":"reject_always"}]});
    validate_permission(&request).unwrap();
    request["options"][1]["optionId"] = json!("peer-1");
    assert!(validate_permission(&request).is_err());
    let prompt = json!({"sessionId":"s","prompt":[{"type":"text","text":"hello"},{"type":"resource_link","name":"file","uri":"file:///workspace/a"}]});
    validate_prompt(&prompt).unwrap();
    let mut malformed = prompt;
    malformed["prompt"]
        .as_array_mut()
        .unwrap()
        .push(json!({"type":"image","data":""}));
    assert!(validate_prompt(&malformed).is_err());
}

#[test]
fn startup_configuration_bounds_and_response_validation_are_exact() {
    use rsi_acp_protocol::configuration::{ConfigSelection, validate};
    let selection = || ConfigSelection {
        id: "model".into(),
        value: "provider/model".into(),
    };
    assert!(validate(&[selection()]).is_ok());
    assert!(validate(&[selection(), selection()]).is_err());
    assert!(
        validate(
            &(0..9)
                .map(|index| ConfigSelection {
                    id: index.to_string(),
                    value: "choice".into()
                })
                .collect::<Vec<_>>()
        )
        .is_err()
    );
    for (id, value) in [
        (String::new(), "choice".into()),
        ("model".into(), String::new()),
        ("a".repeat(257), "choice".into()),
        ("model".into(), "a".repeat(257)),
        ("model".into(), "bad\0choice".into()),
    ] {
        assert!(validate(&[ConfigSelection { id, value }]).is_err());
    }
    for invalid in [
        serde_json::json!({}),
        serde_json::json!({"configOptions":[{"id":"model","name":"Model","type":"select","currentValue":"x","options":[{"value":false,"name":"bad"}]}]}),
    ] {
        assert!(
            rsi_acp_protocol::validate_session_result("session/set_config_option", &invalid)
                .is_err()
        );
    }
    assert!(
        rsi_acp_protocol::validate_session_result(
            "session/set_config_option",
            &serde_json::json!({"configOptions":[]})
        )
        .is_ok()
    );
}
