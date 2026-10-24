use super::*;
use rsi_api_protocol::{
    ApiClient, ApiError, ApiMessage, ApiOutput, ByteBudget, ConnectionDescription, EndpointId,
    HostEpoch, OperationClass, OperationSpec, RetainedBytes,
};
use rsi_configuration_api::*;
use rsi_meta::Execution;
use std::collections::VecDeque;

#[derive(Debug)]
struct ChangingCatalog {
    inner: Arc<Remote>,
    operations: Mutex<Vec<OperationSpec>>,
    failed: std::sync::atomic::AtomicBool,
}
#[async_trait]
impl ApiClient for ChangingCatalog {
    fn description(&self) -> &ConnectionDescription {
        self.inner.description()
    }
    async fn operations(&self) -> rsi_api_protocol::Result<rsi_api_protocol::OperationCatalog> {
        if self.failed.load(std::sync::atomic::Ordering::Relaxed) {
            return Err(ApiError::Capacity);
        }
        rsi_api_protocol::OperationCatalog::new(self.operations.lock().unwrap().clone())
    }
    fn input_budget(&self, class: OperationClass) -> ByteBudget {
        self.inner.input_budget(class)
    }
    async fn call(
        &self,
        operation: &OperationSpec,
        input: RetainedBytes,
    ) -> rsi_api_protocol::Result<ApiOutput> {
        if operation != &ConfigurationOperation::Plugins.spec() {
            return Err(ApiError::UnknownOperation);
        }
        self.inner.call(operation, input).await
    }
}

#[tokio::test]
async fn refresh_hides_and_restores_optional_features_in_the_existing_workbench() {
    let (mut owner, remote) = feature(vec![Ok(page(0, "1")), Ok(page(0, "2")), Ok(page(0, "3"))]);
    let changing = Arc::new(ChangingCatalog {
        inner: remote,
        operations: Mutex::new(vec![]),
        failed: std::sync::atomic::AtomicBool::new(false),
    });
    let mutable = Arc::get_mut(&mut owner).unwrap();
    mutable.api = changing.clone();
    mutable.client = ConfigurationClient::new(changing.clone());
    for present in [true, false, true] {
        *changing.operations.lock().unwrap() = if present {
            vec![ExaOperation::Status.spec()]
        } else {
            vec![]
        };
        owner.command(PluginsCommand::Refresh).await.unwrap();
        assert_eq!(owner.snapshot().exa_available, present);
        assert!(
            owner.snapshot().actions.is_empty(),
            "read availability does not imply writes"
        );
        assert!(!owner.snapshot().ssh.available);
    }
    owner.work.close().await;
}
#[tokio::test]
async fn refresh_updates_individual_write_actions_without_hiding_read_only_status() {
    let (mut owner, remote) = feature(vec![Ok(page(0, "1")), Ok(page(0, "2"))]);
    let changing = Arc::new(ChangingCatalog {
        inner: remote,
        operations: Mutex::new(vec![ExaOperation::Status.spec(), ExaOperation::Set.spec()]),
        failed: std::sync::atomic::AtomicBool::new(false),
    });
    let mutable = Arc::get_mut(&mut owner).unwrap();
    mutable.api = changing.clone();
    mutable.client = ConfigurationClient::new(changing.clone());
    owner.command(PluginsCommand::Refresh).await.unwrap();
    assert!(owner.snapshot().exa_available);
    assert_eq!(owner.snapshot().actions, vec!["exa_set"]);
    changing
        .operations
        .lock()
        .unwrap()
        .retain(|spec| spec == &ExaOperation::Status.spec());
    owner.command(PluginsCommand::Refresh).await.unwrap();
    assert!(owner.snapshot().exa_available);
    assert!(owner.snapshot().actions.is_empty());
    owner.work.close().await;
}
#[tokio::test]
async fn discovery_failure_preserves_capabilities_and_refresh_recovers() {
    let (mut owner, remote) = feature(vec![Ok(page(0, "1")), Ok(page(0, "2")), Ok(page(0, "3"))]);
    let changing = Arc::new(ChangingCatalog {
        inner: remote,
        operations: Mutex::new(vec![ExaOperation::Status.spec()]),
        failed: std::sync::atomic::AtomicBool::new(false),
    });
    let mutable = Arc::get_mut(&mut owner).unwrap();
    mutable.api = changing.clone();
    mutable.client = ConfigurationClient::new(changing.clone());
    owner.command(PluginsCommand::Refresh).await.unwrap();
    changing
        .failed
        .store(true, std::sync::atomic::Ordering::Relaxed);
    owner.command(PluginsCommand::Refresh).await.unwrap();
    assert!(owner.snapshot().exa_available);
    assert!(owner.snapshot().page.is_some());
    changing
        .failed
        .store(false, std::sync::atomic::Ordering::Relaxed);
    changing.operations.lock().unwrap().clear();
    owner.command(PluginsCommand::Refresh).await.unwrap();
    assert!(!owner.snapshot().exa_available);
    owner.work.close().await;
}
#[derive(Debug)]
struct DiscoveryConnection(Arc<ChangingCatalog>);
#[async_trait]
impl PluginFactory for DiscoveryConnection {
    fn prepare(&self, _: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        Ok(PreparedActivation::new(ConfigValue::Null))
    }
    async fn activate(&self, plan: ActivationPlan) -> rsi_meta::Result<()> {
        plan.context()
            .provide_local::<rsi_api_protocol::ApiClientContract>(self.0.clone())?;
        Ok(())
    }
}
#[tokio::test]
async fn factory_survives_initial_discovery_failure_and_refresh_recovers() {
    let (fixture, remote) = feature(vec![Ok(page(0, "1"))]);
    let api = Arc::new(ChangingCatalog {
        inner: remote,
        operations: Mutex::new(vec![ExaOperation::Status.spec()]),
        failed: std::sync::atomic::AtomicBool::new(true),
    });
    let runtime = rsi_meta::Runtime::default();
    let root = runtime.root();
    for (id, factory) in [
        (
            "connection",
            Arc::new(DiscoveryConnection(api.clone())) as Arc<dyn PluginFactory>,
        ),
        ("ui", Arc::new(rsi_ui::UiFactory)),
        ("plugins", Arc::new(PluginsFeatureFactory)),
    ] {
        let owner = root
            .apply(
                rsi_meta::ResolvedFactory::linked(
                    id,
                    "fixture",
                    rsi_meta::UpdateMode::RestartRequired,
                    factory,
                ),
                ConfigValue::Null,
            )
            .await
            .unwrap();
        assert_eq!(owner.snapshot().state, rsi_meta::FiberState::Active);
    }
    let feature = root.lookup_local::<PluginsFeatureContract>().unwrap();
    assert!(feature.snapshot().diagnostic.is_some());
    api.failed
        .store(false, std::sync::atomic::Ordering::Relaxed);
    feature.command(PluginsCommand::Refresh).await.unwrap();
    assert!(feature.snapshot().exa_available);
    assert!(feature.snapshot().diagnostic.is_none());
    feature.work.close().await;
    fixture.work.close().await;
}
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
    async fn operations(&self) -> rsi_api_protocol::Result<rsi_api_protocol::OperationCatalog> {
        rsi_api_protocol::OperationCatalog::new(self.operations.clone())
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
        last_attempt: None,
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
            wire_version: rsi_api_protocol::API_WIRE_VERSION,
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
            client: ConfigurationClient::new(remote.clone()),
            mcp: None,
            mcp_ssh: None,
            exa: None,
            leaves: None,
            ssh: None,
            api: remote.clone(),
            capabilities: Mutex::new(crate::plugins::PluginCapabilities::new(
                &rsi_api_protocol::OperationCatalog::new(remote.operations.clone()).unwrap(),
            )),
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
    async fn operations(&self) -> rsi_api_protocol::Result<rsi_api_protocol::OperationCatalog> {
        rsi_api_protocol::OperationCatalog::new(self.operations.clone())
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
            wire_version: rsi_api_protocol::API_WIRE_VERSION,
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
        mcp_ssh: None,
        exa: Some(ExaClient::new(remote.clone())),
        leaves: None,
        ssh: None,
        api: remote.clone(),
        capabilities: Mutex::new(crate::plugins::PluginCapabilities::new(
            &rsi_api_protocol::OperationCatalog::new(remote.operations.clone()).unwrap(),
        )),
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
