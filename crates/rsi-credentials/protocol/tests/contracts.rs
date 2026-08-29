use rsi_credentials_protocol::{CredentialRef, CredentialSource};
use serde_json::json;

#[test]
fn credential_addresses_and_sources_revalidate_during_deserialization() {
    serde_json::from_value::<CredentialRef>(json!({
        "owner": "rsi.ai.openai",
        "slot": "not a slot"
    }))
    .expect_err("invalid slot must not enter the typed contract");

    serde_json::from_value::<CredentialSource>(json!({
        "kind": "environment",
        "variable": "9INVALID"
    }))
    .expect_err("invalid environment provenance must not enter durable facts");
}
