use super::*;
use crate::{PluginsCommand, Work};
use async_trait::async_trait;
use rsi_api_protocol::{
    ApiClient, ApiError, ApiMessage, ApiOutput, ByteBudget, ConnectionDescription, EndpointId,
    HostEpoch, OperationClass, OperationSpec, RetainedBytes,
};
use std::sync::{Arc, Mutex};
#[derive(Debug)]
struct Remote {
    description: ConnectionDescription,
    operations: Vec<OperationSpec>,
    preview: Preview,
    calls: Mutex<Vec<(String, serde_json::Value)>>,
    entered: tokio::sync::Notify,
    release: tokio::sync::Notify,
}
#[async_trait]
impl ApiClient for Remote {
    fn description(&self) -> &ConnectionDescription {
        &self.description
    }
    fn operations(&self) -> &[OperationSpec] {
        &self.operations
    }
    fn input_budget(&self, _: OperationClass) -> ByteBudget {
        ByteBudget::default()
    }
    async fn call(
        &self,
        operation: &OperationSpec,
        input: RetainedBytes,
    ) -> rsi_api_protocol::Result<ApiOutput> {
        let name = operation.id.name();
        self.calls
            .lock()
            .unwrap()
            .push((name.into(), serde_json::from_slice(input.as_ref()).unwrap()));
        let value = match name {
            "preview" => serde_json::to_value(&self.preview).unwrap(),
            "commit" => {
                self.entered.notify_one();
                self.release.notified().await;
                return Err(ApiError::OutcomeUnknown);
            }
            "receipt" => serde_json::to_value(Receipt {
                preview: self.preview.clone(),
                outcome: Outcome::Saved {
                    directory_synced: false,
                    application: wire::Application::NotSelected,
                },
            })
            .unwrap(),
            _ => panic!("unexpected wire operation {name}"),
        };
        Ok(ApiOutput::Reply(ApiMessage {
            json: ByteBudget::default()
                .encode(&value, operation.maximum_response_bytes)
                .unwrap(),
            binary: None,
        }))
    }
}
fn fixture() -> (Arc<PluginsFeature>, Arc<Remote>) {
    let epoch = HostEpoch::from_bytes([2; 16]);
    let remote = Arc::new(Remote {
        description: ConnectionDescription {
            wire_version: 1,
            endpoint_id: EndpointId::from_bytes([1; 16]),
            host_epoch: epoch.clone(),
        },
        operations: [
            wire::Operation::Catalog,
            wire::Operation::Preview,
            wire::Operation::Previews,
            wire::Operation::Commit,
            wire::Operation::Receipt,
            wire::Operation::Receipts,
            wire::Operation::Discard,
        ]
        .into_iter()
        .map(wire::Operation::spec)
        .chain([
            rsi_configuration_api::ConfigurationOperation::Status.spec(),
            rsi_configuration_api::ConfigurationOperation::Plugins.spec(),
        ])
        .collect(),
        preview: Preview {
            host_epoch: epoch,
            ticket: "a".repeat(32),
            target: Target {
                root: "b".repeat(64),
                profile: "editable".into(),
                leaf: "fixture".into(),
            },
            operation: wire::ChangeKind::Configuration,
            digest: "c".repeat(64),
            source_digest: "d".repeat(64),
            plugin: "fixture".into(),
            previous_enabled: true,
            enabled: true,
            effective_enabled: true,
        },
        calls: Mutex::default(),
        entered: tokio::sync::Notify::new(),
        release: tokio::sync::Notify::new(),
    });
    let owner = Arc::new(PluginsFeature {
        client: rsi_configuration_api::ConfigurationClient::new(remote.clone()).unwrap(),
        mcp: None,
        exa: None,
        leaves: Some(wire::Client::new(remote.clone()).unwrap()),
        leaf_grants: false,
        state: Mutex::default(),
        work: Work::new(rsi_meta::Execution::native(
            tokio::runtime::Handle::current(),
        )),
    });
    (owner, remote)
}
fn command(value: LeafCommand) -> PluginsCommand {
    PluginsCommand::Leaves { command: value }
}
#[tokio::test]
async fn exact_config_is_ephemeral_and_unknown_commit_keeps_original_receipt_without_replay() {
    let (owner, remote) = fixture();
    let preview = &remote.preview;
    owner
        .command(command(LeafCommand::Configuration {
            target: preview.target.clone(),
            document: r#"{"number":1e+400,"null":null,"secret":"private-input"}"#.into(),
        }))
        .await
        .unwrap();
    {
        let calls = remote.calls.lock().unwrap();
        assert_eq!(
            calls[0].1["change"]["value"]["number"]
                .as_number()
                .unwrap()
                .as_str(),
            "1e+400"
        );
        assert!(calls[0].1["change"]["value"]["null"].is_null());
    }
    assert!(
        !serde_json::to_string(&owner.snapshot())
            .unwrap()
            .contains("private-input")
    );
    let mut changes = owner.changes();
    let waiter = owner.command(command(LeafCommand::Commit {
        ticket: preview.ticket.clone(),
        digest: preview.digest.clone(),
    }));
    remote.entered.notified().await;
    drop(waiter);
    remote.release.notify_one();
    changes.changed().await.unwrap();
    assert!(matches!(
        owner.snapshot().leaves.receipt.unwrap().outcome,
        Outcome::Unknown
    ));
    assert!(
        owner
            .command(command(LeafCommand::Commit {
                ticket: preview.ticket.clone(),
                digest: preview.digest.clone()
            }))
            .await
            .is_err()
    );
    assert_eq!(remote.calls.lock().unwrap().len(), 2);
    owner
        .command(command(LeafCommand::Receipt {
            ticket: preview.ticket.clone(),
        }))
        .await
        .unwrap();
    let view = owner.snapshot().leaves;
    assert!(matches!(
        view.receipt.unwrap().outcome,
        Outcome::Saved {
            directory_synced: false,
            application: wire::Application::NotSelected
        }
    ));
    assert!(
        view.previews.is_empty(),
        "settled source operation cannot be saved from an old prepared card"
    );
    assert_eq!(
        remote
            .calls
            .lock()
            .unwrap()
            .iter()
            .filter(|(name, _)| name == "commit")
            .count(),
        1
    );
    owner.work.close().await;
}
