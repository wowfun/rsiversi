use super::*;
use rsi_api_protocol::{
    ApiClient, ApiError, ApiMessage, ApiOutput, ByteBudget, ConnectionDescription, EndpointId,
    HostEpoch, OperationClass, OperationSpec, RetainedBytes,
};
use rsi_configuration_api::*;
use rsi_meta::Execution;
use std::collections::VecDeque;
#[derive(Debug)]
struct Remote {
    description: ConnectionDescription,
    operations: Vec<OperationSpec>,
    results: Mutex<VecDeque<rsi_api_protocol::Result<PluginStatusPage>>>,
    calls: Mutex<Vec<PluginStatusRequest>>,
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
        assert_eq!(operation, &ConfigurationOperation::Plugins.spec());
        self.calls
            .lock()
            .unwrap()
            .push(serde_json::from_slice(input.as_ref()).unwrap());
        let page = self.results.lock().unwrap().pop_front().unwrap()?;
        Ok(ApiOutput::Reply(ApiMessage {
            json: ByteBudget::default()
                .encode(&page, operation.maximum_response_bytes)
                .unwrap(),
            binary: None,
        }))
    }
}
fn page(offset: usize, observed: &str) -> PluginStatusPage {
    PluginStatusPage {
        context: PluginStatusContext::default(),
        desired_revision: "3".into(),
        observed_revision: observed.into(),
        health: Some(PluginHealth::Converged),
        watcher: Some(PluginWatcher::Inactive),
        offset,
        total: 33,
        next_offset: (offset == 0).then_some(32),
        plugins: (offset..33.min(offset + 32))
            .map(|i| PluginStatusRow {
                instance: format!("plugin-{i:02}"),
                desired_plugin: Some("fixture".into()),
                enabled: true,
                origin: PluginOrigin::Linked,
                diagnostics: vec![],
                observed: None,
            })
            .collect(),
    }
}
fn feature(
    results: Vec<rsi_api_protocol::Result<PluginStatusPage>>,
) -> (Arc<PluginsFeature>, Arc<Remote>) {
    let remote = Arc::new(Remote {
        description: ConnectionDescription {
            wire_version: 1,
            endpoint_id: EndpointId::from_bytes([1; 16]),
            host_epoch: HostEpoch::from_bytes([2; 16]),
        },
        operations: vec![
            ConfigurationOperation::Status.spec(),
            ConfigurationOperation::Plugins.spec(),
        ],
        results: Mutex::new(results.into()),
        calls: Mutex::default(),
    });
    (
        Arc::new(PluginsFeature {
            client: ConfigurationClient::new(remote.clone()).unwrap(),
            mcp: None,
            exa: None,
            leaves: None,
            leaf_grants: false,
            state: Mutex::default(),
            work: Work::new(Execution::native(tokio::runtime::Handle::current())),
        }),
        remote,
    )
}
#[tokio::test]
async fn stale_ticket_does_not_read_and_changed_revision_clears_old_page() {
    let (owner, remote) = feature(vec![
        Ok(page(0, "9")),
        Ok(page(32, "10")),
        Ok(page(0, "10")),
    ]);
    owner.command(PluginsCommand::Refresh).await.unwrap();
    let ticket = owner.snapshot().ticket;
    assert!(
        owner
            .command(PluginsCommand::Page {
                ticket: "stale".into(),
                offset: 32
            })
            .await
            .is_err()
    );
    assert_eq!(remote.calls.lock().unwrap().len(), 1);
    assert!(
        owner
            .command(PluginsCommand::Page { ticket, offset: 32 })
            .await
            .is_err()
    );
    let failed = owner.snapshot();
    assert!(failed.page.is_none());
    assert!(failed.diagnostic.unwrap().contains("changed while paging"));
    owner.command(PluginsCommand::Refresh).await.unwrap();
    assert_eq!(owner.snapshot().page.unwrap().offset, 0);
    owner.work.close().await;
    assert!(owner.command(PluginsCommand::Refresh).await.is_err());
    assert_eq!(remote.calls.lock().unwrap().len(), 3);
}
#[tokio::test]
async fn unauthorized_refresh_never_leaves_stale_observations_visible() {
    let (owner, _) = feature(vec![Ok(page(0, "1")), Err(ApiError::Unauthorized)]);
    owner.command(PluginsCommand::Refresh).await.unwrap();
    assert!(owner.snapshot().page.is_some());
    assert!(owner.command(PluginsCommand::Refresh).await.is_err());
    assert!(owner.snapshot().page.is_none());
    assert!(owner.snapshot().diagnostic.is_some());
    owner.work.close().await;
}

#[derive(Debug)]
struct CredentialRemote {
    description: ConnectionDescription,
    operations: Vec<OperationSpec>,
    calls: Mutex<Vec<String>>,
    entered: tokio::sync::Notify,
    release: tokio::sync::Notify,
}
#[async_trait]
impl ApiClient for CredentialRemote {
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
        self.calls.lock().unwrap().push(operation.id.name().into());
        if operation == &ExaOperation::Set.spec() {
            let input: serde_json::Value = serde_json::from_slice(input.as_ref()).unwrap();
            assert_eq!(input["secret"], "ephemeral-exa-test-value");
            self.entered.notify_one();
            self.release.notified().await;
            return Err(ApiError::OutcomeUnknown);
        }
        assert_eq!(operation, &ExaOperation::Status.spec());
        Ok(ApiOutput::Reply(ApiMessage {
            json: ByteBudget::default()
                .encode(
                    &ExaCredentialStatus {
                        availability: rsi_credentials_protocol::CredentialAvailability::Missing,
                        editable: true,
                    },
                    operation.maximum_response_bytes,
                )
                .unwrap(),
            binary: None,
        }))
    }
}
#[tokio::test]
async fn abandoned_exa_write_holds_admission_clears_observation_and_is_not_replayed_on_close() {
    let (configuration, _) = feature(vec![]);
    let remote = Arc::new(CredentialRemote {
        description: ConnectionDescription {
            wire_version: 1,
            endpoint_id: EndpointId::from_bytes([1; 16]),
            host_epoch: HostEpoch::from_bytes([2; 16]),
        },
        operations: [ExaOperation::Status, ExaOperation::Set, ExaOperation::Unset]
            .map(ExaOperation::spec)
            .into(),
        calls: Mutex::default(),
        entered: tokio::sync::Notify::new(),
        release: tokio::sync::Notify::new(),
    });
    let owner = Arc::new(PluginsFeature {
        client: configuration.client.clone(),
        mcp: None,
        exa: Some(ExaClient::new(remote.clone()).unwrap()),
        leaves: None,
        leaf_grants: false,
        state: Mutex::default(),
        work: Work::new(Execution::native(tokio::runtime::Handle::current())),
    });
    owner.command(PluginsCommand::ExaStatus).await.unwrap();
    assert!(owner.snapshot().exa_credential.is_some());
    let waiter = owner.command(PluginsCommand::ExaSet {
        secret: rsi_credentials_protocol::SecretValue::new("ephemeral-exa-test-value").unwrap(),
    });
    remote.entered.notified().await;
    drop(waiter);
    assert!(owner.snapshot().exa_credential.is_none());
    assert!(
        !serde_json::to_string(&owner.snapshot())
            .unwrap()
            .contains("ephemeral-exa-test-value")
    );
    assert!(owner.command(PluginsCommand::ExaUnset).await.is_err());
    let close = owner.work.close();
    tokio::pin!(close);
    assert!(futures_util::poll!(close.as_mut()).is_pending());
    remote.release.notify_one();
    close.await;
    assert!(owner.snapshot().exa_credential.is_none());
    assert!(owner.snapshot().exa_notice.unwrap().contains("unknown"));
    assert!(owner.command(PluginsCommand::ExaStatus).await.is_err());
    assert_eq!(*remote.calls.lock().unwrap(), ["status", "set"]);
    configuration.work.close().await;
}

#[tokio::test]
async fn selected_source_survives_refresh_and_digest_changes_fence_pagination() {
    let target = PluginStatusTarget::Preset {
        id: "review".into(),
    };
    let mut first = page(0, "0");
    first.context.target = target.clone();
    first.context.source_digest = Some("a".repeat(64));
    first.health = None;
    first.watcher = None;
    let mut changed = first.clone();
    changed.offset = 32;
    changed.plugins = vec![page(32, "0").plugins.remove(0)];
    changed.next_offset = None;
    changed.context.source_digest = Some("b".repeat(64));
    let mut refreshed = first.clone();
    refreshed.context.source_digest = changed.context.source_digest.clone();
    let (owner, remote) = feature(vec![Ok(first), Ok(changed), Ok(refreshed)]);
    owner
        .command(PluginsCommand::Select {
            target: target.clone(),
        })
        .await
        .unwrap();
    let ticket = owner.snapshot().ticket;
    assert!(
        owner
            .command(PluginsCommand::Page { ticket, offset: 32 })
            .await
            .is_err()
    );
    assert!(owner.snapshot().page.is_none());
    owner.command(PluginsCommand::Refresh).await.unwrap();
    assert_eq!(owner.snapshot().target, target);
    assert!(
        remote
            .calls
            .lock()
            .unwrap()
            .iter()
            .all(|request| request.target == target)
    );
}
