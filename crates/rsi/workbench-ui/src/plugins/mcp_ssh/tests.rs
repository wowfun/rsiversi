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
    mutations: Mutex<usize>,
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
        if operation.id.name() != "ssh-get" {
            *self.mutations.lock().unwrap() += 1;
            return Err(ApiError::OutcomeUnknown);
        }
        let target = serde_json::from_slice(input.as_bytes()).unwrap();
        let reply = wire::State {
            target,
            revision: "1".into(),
            config: None,
            apply_error: None,
        };
        Ok(ApiOutput::Reply(ApiMessage {
            json: ByteBudget::default()
                .encode(&reply, operation.maximum_response_bytes)
                .unwrap(),
            binary: None,
        }))
    }
}
#[tokio::test]
async fn exact_ssh_mcp_editor_requires_current_read_and_unknown_mutation_never_replays() {
    let remote = Arc::new(Remote {
        description: ConnectionDescription {
            wire_version: 1,
            endpoint_id: EndpointId::from_bytes([1; 16]),
            host_epoch: HostEpoch::from_bytes([2; 16]),
        },
        operations: [
            wire::Operation::Get,
            wire::Operation::Put,
            wire::Operation::Remove,
            wire::Operation::Refresh,
        ]
        .into_iter()
        .map(wire::Operation::spec)
        .chain([
            rsi_configuration_api::ConfigurationOperation::Status.spec(),
            rsi_configuration_api::ConfigurationOperation::Plugins.spec(),
        ])
        .collect(),
        mutations: Mutex::new(0),
    });
    let owner = Arc::new(PluginsFeature {
        client: rsi_configuration_api::ConfigurationClient::new(remote.clone()).unwrap(),
        mcp: None,
        mcp_ssh: Some(wire::Client::new(remote.clone())),
        exa: None,
        leaves: None,
        leaf_grants: false,
        ssh: None,
        ssh_trust: false,
        state: Mutex::default(),
        work: Work::new(rsi_meta::Execution::native(
            tokio::runtime::Handle::current(),
        )),
    });
    let target:wire::Target=serde_json::from_value(serde_json::json!({"host_epoch":remote.description.host_epoch,"target":"a".repeat(32),"server":"fixture"})).unwrap();
    let change = wire::Change {
        target: target.clone(),
        expected: "1".into(),
        config: None,
    };
    let wrap = |command| PluginsCommand::McpSsh { command };
    assert!(
        owner
            .command(wrap(McpSshCommand::Remove {
                change: change.clone()
            }))
            .await
            .is_err()
    );
    assert_eq!(*remote.mutations.lock().unwrap(), 0);
    owner
        .command(wrap(McpSshCommand::Read {
            target: target.clone(),
        }))
        .await
        .unwrap();
    let mut foreign = change.clone();
    foreign.target.server = "another".into();
    assert!(
        owner
            .command(wrap(McpSshCommand::Remove { change: foreign }))
            .await
            .is_err()
    );
    assert_eq!(*remote.mutations.lock().unwrap(), 0);
    assert!(
        owner
            .command(wrap(McpSshCommand::Remove {
                change: change.clone()
            }))
            .await
            .is_err()
    );
    assert!(owner.snapshot().mcp_ssh.uncertain);
    assert!(owner.snapshot().mcp_ssh.state.is_none());
    assert!(
        owner
            .command(wrap(McpSshCommand::Remove {
                change: change.clone()
            }))
            .await
            .is_err()
    );
    assert_eq!(*remote.mutations.lock().unwrap(), 1);
    owner
        .command(wrap(McpSshCommand::Read { target }))
        .await
        .unwrap();
    assert!(!owner.snapshot().mcp_ssh.uncertain);
    assert_eq!(*remote.mutations.lock().unwrap(), 1);
    assert!(
        owner
            .command(wrap(McpSshCommand::Refresh { change }))
            .await
            .is_err()
    );
    assert_eq!(*remote.mutations.lock().unwrap(), 2);
    owner.work.close().await;
}
