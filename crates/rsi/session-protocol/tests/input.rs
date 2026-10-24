use rsi_session_protocol::{SessionInput, ValidatedSessionInput};

#[test]
fn local_and_wire_inputs_share_exact_boundaries_and_array_encoding() {
    let maximum = rsi_agent_session_protocol::MAXIMUM_TURN_TEXT_BYTES;
    for (text, accepted) in [
        ("x".into(), true),
        ("界".into(), true),
        ("x".repeat(maximum), true),
        ("x".repeat(maximum + 1), false),
        (String::new(), false),
        ("bad\0text".into(), false),
        ("bad\u{7f}text".into(), false),
    ] {
        let content = vec![SessionInput::Text { text }];
        let wire = serde_json::to_value(&content).unwrap();
        let local = ValidatedSessionInput::try_from(content);
        assert_eq!(local.is_ok(), accepted);
        let decoded = serde_json::from_value::<ValidatedSessionInput>(wire.clone());
        assert_eq!(decoded.is_ok(), accepted);
        if let Ok(local) = local {
            assert_eq!(decoded.unwrap(), local);
            assert_eq!(serde_json::to_value(local).unwrap(), wire);
        }
    }
    assert!(ValidatedSessionInput::try_from(Vec::new()).is_err());
    assert!(serde_json::from_str::<ValidatedSessionInput>("[]").is_err());
    let max = rsi_agent_session_protocol::MAXIMUM_AGENT_MESSAGE_CONTENT_BLOCKS;
    for count in [max, max + 1] {
        let blocks = vec![SessionInput::Text { text: "x".into() }; count];
        let wire = serde_json::to_value(&blocks).unwrap();
        assert_eq!(
            ValidatedSessionInput::try_from(blocks).is_ok(),
            count == max
        );
        assert_eq!(
            serde_json::from_value::<ValidatedSessionInput>(wire).is_ok(),
            count == max
        );
    }
}
