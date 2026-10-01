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
        _: RetainedBytes,
    ) -> rsi_api_protocol::Result<ApiOutput> {
        if operation.id.name() != "catalog" {
            *self.mutations.lock().unwrap() += 1;
            return Err(ApiError::OutcomeUnknown);
        }
        Ok(ApiOutput::Reply(ApiMessage {
            json: ByteBudget::default()
                .encode(
                    &wire::Catalog {
                        host_epoch: self.description.host_epoch.clone(),
                        targets: vec![],
                    },
                    operation.maximum_response_bytes,
                )
                .unwrap(),
            binary: None,
        }))
    }
}
#[tokio::test]
async fn unknown_ssh_mutation_requires_explicit_read_and_never_replays() {
    let remote = Arc::new(Remote {
        description: ConnectionDescription {
            wire_version: 1,
            endpoint_id: EndpointId::from_bytes([1; 16]),
            host_epoch: HostEpoch::from_bytes([2; 16]),
        },
        operations: [
            wire::Operation::Catalog,
            wire::Operation::ResolveDirectory,
            wire::Operation::PutCandidate,
            wire::Operation::Connect,
            wire::Operation::Disconnect,
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
        mcp_ssh: None,
        exa: None,
        leaves: None,
        leaf_grants: false,
        ssh: Some(wire::Client::new(remote.clone()).unwrap()),
        ssh_trust: false,
        state: Mutex::default(),
        work: Work::new(rsi_meta::Execution::native(
            tokio::runtime::Handle::current(),
        )),
    });
    let request: wire::PutCandidate = serde_json::from_value(serde_json::json!({"host_epoch":remote.description.host_epoch,"expected":"0","candidate":{"target":"a".repeat(32),"name":"target","endpoint":{"host":"host","port":22,"user":"fixture"}}})).unwrap();
    let command = || PluginsCommand::Ssh {
        command: SshCommand::Put {
            request: request.clone(),
        },
    };
    assert!(owner.command(command()).await.is_err());
    assert!(owner.snapshot().ssh.uncertain);
    assert!(owner.command(command()).await.is_err());
    assert_eq!(*remote.mutations.lock().unwrap(), 1);
    owner
        .command(PluginsCommand::Ssh {
            command: SshCommand::Read,
        })
        .await
        .unwrap();
    assert!(!owner.snapshot().ssh.uncertain);
    assert_eq!(*remote.mutations.lock().unwrap(), 1);
    assert!(!owner.snapshot().ssh.can_trust);
    assert!(owner.command(command()).await.is_err());
    assert_eq!(*remote.mutations.lock().unwrap(), 2);
    owner.work.close().await;
}
