use async_trait::async_trait;
use rsi_agent_session_protocol::AgentPresetId;
use rsi_api_protocol::{
    ApiClient, ApiError, ApiMessage, ApiOutput, ByteBudget, ConnectionDescription,
    OperationCatalog, OperationClass, OperationSpec, Result, RetainedBytes,
};
use rsi_configuration_api::agent_presets::{
    AgentPresetDocumentError, AgentPresetsClient, AgentPresetsOperation,
};
use serde_json::{Value, json};
use std::sync::Arc;

#[derive(Debug)]
struct Reply {
    description: ConnectionDescription,
    value: Value,
    domain_error: bool,
}

#[async_trait]
impl ApiClient for Reply {
    fn description(&self) -> &ConnectionDescription {
        &self.description
    }
    async fn operations(&self) -> Result<OperationCatalog> {
        OperationCatalog::new(vec![
            AgentPresetsOperation::List.spec(),
            AgentPresetsOperation::Document.spec(),
        ])
    }
    fn input_budget(&self, _: OperationClass) -> ByteBudget {
        ByteBudget::default()
    }
    async fn call(&self, spec: &OperationSpec, _: RetainedBytes) -> Result<ApiOutput> {
        let json = ByteBudget::default().encode(&self.value, spec.maximum_response_bytes)?;
        if self.domain_error {
            Err(ApiError::Domain(json))
        } else {
            Ok(ApiOutput::Reply(ApiMessage { json, binary: None }))
        }
    }
}

fn client(value: Value, domain_error: bool) -> AgentPresetsClient {
    AgentPresetsClient::new(Arc::new(Reply {
        description: ConnectionDescription {
            wire_version: 1,
            endpoint_id: rsi_api_protocol::EndpointId::from_bytes([1; 16]),
            host_epoch: rsi_api_protocol::HostEpoch::from_bytes([2; 16]),
        },
        value,
        domain_error,
    }))
}

#[tokio::test]
async fn list_rejects_ambiguous_identity_and_default_responses() {
    let row =
        json!({"id":"standard","is_default":true,"name":null,"description":null,"broken":null});
    assert_eq!(
        client(json!({"presets":[row.clone()]}), false)
            .list()
            .await
            .unwrap()
            .presets
            .len(),
        1
    );
    let mut other = row.clone();
    other["id"] = json!("alternate");
    let mut capacity: Vec<_> = (0..256)
        .map(|index| {
            json!({
                "id":format!("preset-{index}"), "is_default":false,
                "name":null, "description":null, "broken":null,
            })
        })
        .collect();
    assert_eq!(
        client(json!({"presets":capacity}), false)
            .list()
            .await
            .unwrap()
            .presets
            .len(),
        256
    );
    capacity.push(row.clone());
    for rows in [vec![row.clone(), row.clone()], vec![row, other], capacity] {
        assert!(matches!(
            client(json!({"presets":rows}), false).list().await,
            Err(ApiError::Invalid(_))
        ));
    }
}

#[tokio::test]
async fn document_checks_response_identity_and_preserves_definite_lookup_errors() {
    let id = AgentPresetId::new("standard").unwrap();
    let doc = json!({"id":"standard","format":"toml","content":"format = 1\n","name":null,"description":null});
    assert_eq!(
        client(doc.clone(), false)
            .document(&id)
            .await
            .unwrap()
            .unwrap()
            .content,
        "format = 1\n"
    );
    for (field, value) in [("id", "alternate"), ("format", "yaml")] {
        let mut wrong = doc.clone();
        wrong[field] = json!(value);
        assert!(matches!(
            client(wrong, false).document(&id).await,
            Err(ApiError::Invalid(_))
        ));
    }
    assert!(matches!(
        client(json!({"code":"not_found"}), true)
            .document(&id)
            .await
            .unwrap(),
        Err(AgentPresetDocumentError::NotFound)
    ));
    assert!(
        matches!(client(json!({"code":"broken","reason":"invalid source"}), true).document(&id).await.unwrap(), Err(AgentPresetDocumentError::Broken { reason }) if reason == "invalid source")
    );
}
