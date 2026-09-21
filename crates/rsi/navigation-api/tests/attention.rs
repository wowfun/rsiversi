use rsi_navigation_api::attention::{Page, Position};
use serde_json::json;
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
    let page = json!({"host_epoch":rsi_api_protocol::HostEpoch::generate().unwrap(),"truncated":false,"entries":[{"position":position,"status":"waiting","targets":[{"kind":"external","generation":"1","request":"permission-exact"}]}]});
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
