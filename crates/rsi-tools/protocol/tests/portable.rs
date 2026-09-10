use rsi_tools_protocol::portable::{self, OsValue, Request, Response};
use serde_json::json;

#[test]
fn frames_reject_extra_fields_duplicate_keys_invalid_numbers_and_oversized_buffers() {
    for source in [
        r#"{"op":"describe","secret":true}"#,
        r#"{"op":"describe","op":"describe"}"#,
        r#"{"op":"confine","program":"/bin/echo","arguments":[],"mode":"danger-full-access"}"#,
        r#"{"op":"result","result":{"value":null,"content":[{"type":"text","text":"ok","secret":true}],"is_error":false,"enforcement":[]}}"#,
        r#"{"op":"result","result":{"value":1.0000000000000001,"content":[],"is_error":false,"enforcement":[]}}"#,
    ] {
        assert!(
            portable::decode::<Request>(source.as_bytes()).is_err(),
            "request accepted {source}"
        );
        assert!(
            portable::decode::<Response>(source.as_bytes()).is_err(),
            "response accepted {source}"
        );
    }
    assert!(portable::decode::<Request>(&vec![b' '; portable::MAXIMUM_FRAME_BYTES + 1]).is_err());
    let exact = "a".repeat(portable::MAXIMUM_FRAME_BYTES - 2);
    let bytes = portable::encode(&exact).unwrap();
    assert_eq!(bytes.len(), portable::MAXIMUM_FRAME_BYTES);
    assert_eq!(bytes.capacity(), portable::MAXIMUM_FRAME_BYTES);
    assert_eq!(portable::decode::<String>(&bytes).unwrap(), exact);
    assert!(portable::encode(&format!("{exact}x")).is_err());
    assert!(
        portable::decode::<Request>(&portable::encode(&json!({"op":"describe"})).unwrap()).is_ok()
    );
}

#[cfg(unix)]
#[test]
fn process_plans_preserve_non_utf8_os_strings_and_reject_foreign_platform() {
    use std::os::unix::ffi::OsStringExt as _;
    let original = std::ffi::OsString::from_vec(vec![b'/', 0xff, b'x']);
    let captured = OsValue::capture(&original).unwrap();
    let decoded: OsValue = portable::decode(&portable::encode(&captured).unwrap()).unwrap();
    assert_eq!(decoded.restore().unwrap(), original);
    assert!(OsValue::Windows(vec![0xd800]).restore().is_err());
}

#[cfg(windows)]
#[test]
fn process_plans_preserve_wide_os_strings_and_reject_foreign_platform() {
    use std::os::windows::ffi::OsStringExt as _;
    let original = std::ffi::OsString::from_wide(&[b'C' as u16, 0xd800]);
    let captured = OsValue::capture(&original).unwrap();
    let decoded: OsValue = portable::decode(&portable::encode(&captured).unwrap()).unwrap();
    assert_eq!(decoded.restore().unwrap(), original);
    assert!(OsValue::Unix(vec![0xff]).restore().is_err());
}
