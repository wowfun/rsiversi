use rsi_navigation_api::attention::{Operation, Page, Position};
use serde_json::json;

#[test]
fn mark_admits_maximal_positions_with_half_a_kibibyte_of_response_capacity() {
    for (kind, id, epoch, sequence) in [
        (
            "native",
            "s".repeat(256),
            "0".to_string(),
            u64::MAX.to_string(),
        ),
        (
            "external",
            "e".repeat(128),
            i64::MAX.to_string(),
            i64::MAX.to_string(),
        ),
    ] {
        let position: Position = serde_json::from_value(json!({
            "conversation": {"kind": kind, "id": id}, "epoch": epoch, "sequence": sequence
        }))
        .unwrap();
        position.validate().unwrap();
        let budget = rsi_api_protocol::ByteBudget::new(512).unwrap();
        let encoded = budget
            .reserve(Operation::Mark.spec().maximum_response_bytes)
            .unwrap()
            .encode(&position)
            .unwrap();
        assert_eq!(
            serde_json::from_slice::<Position>(encoded.as_bytes()).unwrap(),
            position
        );
    }
}

#[test]
fn attention_wire_preserves_coordinates_and_rejects_backend_and_request_ambiguity() {
    let position = json!({"conversation":{"kind":"external","id":"same"},"epoch":"1","sequence":"9007199254740993"});
    let valid: Position = serde_json::from_value(position.clone()).unwrap();
    valid.validate().unwrap();
    assert_eq!(serde_json::to_value(valid).unwrap(), position);
    for (field, value) in [
        ("epoch", json!("0")),
        ("epoch", json!("01")),
        ("sequence", json!("9223372036854775808")),
        ("sequence", json!(3)),
    ] {
        let mut wrong = position.clone();
        wrong[field] = value;
        assert!(
            !serde_json::from_value::<Position>(wrong)
                .is_ok_and(|position| position.validate().is_ok())
        );
    }
    let page = json!({"host_epoch":rsi_api_protocol::HostEpoch::generate().unwrap(),"truncated":false,"entries":[{"position":position,"status":"waiting","reading":{"manual":false,"unseen":false,"failed":false},"targets":[{"kind":"external","generation":"1","request":"permission-exact"}]}]});
    serde_json::from_value::<Page>(page.clone())
        .unwrap()
        .validate()
        .unwrap();
    let mut wrong = page.clone();
    wrong["entries"][0]["position"]["conversation"]["kind"] = json!("native");
    wrong["entries"][0]["position"]["epoch"] = json!("0");
    assert!(
        serde_json::from_value::<Page>(wrong)
            .unwrap()
            .validate()
            .is_err()
    );
    let mut wrong = page.clone();
    wrong["entries"][0]["targets"] = json!([]);
    assert!(
        serde_json::from_value::<Page>(wrong)
            .unwrap()
            .validate()
            .is_err()
    );
    let mut wrong = page.clone();
    wrong["entries"]
        .as_array_mut()
        .unwrap()
        .push(page["entries"][0].clone());
    assert!(
        serde_json::from_value::<Page>(wrong)
            .unwrap()
            .validate()
            .is_err()
    );
}
