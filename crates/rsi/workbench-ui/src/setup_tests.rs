use super::*;
use crate::Work;
use rsi_api_protocol::{
    ApiClient, ApiError, ApiMessage, ApiOutput, ByteBudget, ConnectionDescription, EndpointId,
    HostEpoch, OperationClass, OperationSpec, RetainedBytes,
};
use rsi_configuration_api::{ConfigurationOperation, CredentialOperation, ProvidersOperation};
use rsi_settings_protocol::{SettingsDescription, SettingsPage, SettingsVersion};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

#[derive(Debug)]
struct Remote {
    description: ConnectionDescription,
    operations: Vec<OperationSpec>,
    result: Mutex<Option<rsi_api_protocol::Result<serde_json::Value>>>,
    writes: AtomicUsize,
    discovery_failed: std::sync::atomic::AtomicBool,
    reads: AtomicUsize,
}
#[async_trait::async_trait]
impl ApiClient for Remote {
    fn description(&self) -> &ConnectionDescription {
        &self.description
    }
    async fn operations(&self) -> rsi_api_protocol::Result<rsi_api_protocol::OperationCatalog> {
        if self.discovery_failed.load(Ordering::SeqCst) {
            return Err(ApiError::Capacity);
        }
        rsi_api_protocol::OperationCatalog::new(self.operations.clone())
    }
    fn input_budget(&self, _: OperationClass) -> ByteBudget {
        ByteBudget::default()
    }
    async fn call(
        &self,
        operation: &OperationSpec,
        _: RetainedBytes,
    ) -> rsi_api_protocol::Result<ApiOutput> {
        if operation == &ConfigurationOperation::Status.spec() {
            self.reads.fetch_add(1, Ordering::SeqCst);
            return Ok(ApiOutput::Reply(ApiMessage {
                json: ByteBudget::default()
                    .encode(
                        &serde_json::json!({"allowed":true}),
                        operation.maximum_response_bytes,
                    )
                    .unwrap(),
                binary: None,
            }));
        }
        if operation == &rsi_ai_models_api::describe_operation() {
            self.reads.fetch_add(1, Ordering::SeqCst);
        } else {
            assert_eq!(operation, &ProvidersOperation::Replace.spec());
            self.writes.fetch_add(1, Ordering::SeqCst);
        }
        let value = self
            .result
            .lock()
            .unwrap()
            .take()
            .expect("one explicit write")?;
        Ok(ApiOutput::Reply(ApiMessage {
            json: ByteBudget::default()
                .encode(&value, operation.maximum_response_bytes)
                .unwrap(),
            binary: None,
        }))
    }
}

#[derive(Debug)]
struct DefaultSettings(Mutex<Value>);
#[async_trait::async_trait]
impl SettingsAccess for DefaultSettings {
    async fn list(&self, _: Option<&str>, _: usize) -> rsi_settings_protocol::Result<SettingsPage> {
        unreachable!()
    }
    async fn describe(&self, _: &str) -> rsi_settings_protocol::Result<SettingsDescription> {
        unreachable!()
    }
    async fn read(&self, namespace: &str) -> rsi_settings_protocol::Result<SettingsSnapshot> {
        Ok(SettingsSnapshot {
            scope_id: rsi_settings_protocol::SettingsScopeId::parse("a".repeat(32)).unwrap(),
            revision: 1,
            value: if namespace == "rsi.agent" {
                self.0.lock().unwrap().clone()
            } else {
                json!({})
            },
        })
    }
    async fn replace(
        &self,
        namespace: &str,
        expected: &SettingsVersion,
        value: Value,
    ) -> rsi_settings_protocol::Result<SettingsSnapshot> {
        assert_eq!(namespace, "rsi.agent");
        assert_eq!(expected, &self.read(namespace).await?.version());
        *self.0.lock().unwrap() = value;
        self.read(namespace).await
    }
    async fn clear(
        &self,
        _: &str,
        _: &SettingsVersion,
    ) -> rsi_settings_protocol::Result<SettingsSnapshot> {
        unreachable!()
    }
}

fn model_with_optional_reasoning(
    model: rsi_ai_protocol::ModelRef,
) -> rsi_ai_protocol::LanguageModelDescription {
    use rsi_ai_protocol::{
        ImageToolResultCapability, LanguageModelDescription, LanguageProfile, ReasoningEffortId,
        ReasoningEffortProfile, ToolDialect,
    };
    let profile = LanguageProfile::new(
        10000,
        1000,
        2000,
        ToolDialect::ChatCompletions,
        false,
        ImageToolResultCapability::No,
        vec![],
    )
    .unwrap()
    .with_reasoning_efforts(
        ReasoningEffortProfile::new(
            vec![
                ReasoningEffortId::new("off").unwrap(),
                ReasoningEffortId::new("high").unwrap(),
            ],
            Some(ReasoningEffortId::new("off").unwrap()),
        )
        .unwrap(),
    );
    LanguageModelDescription::new(model, profile, 1, "fixture", "chat", "https", "fixture").unwrap()
}

#[tokio::test]
async fn setup_describes_default_model_before_a_session_and_clears_stale_capabilities() {
    use rsi_ai_protocol::{ModelRef, ReasoningEffortId};
    let model = ModelRef::new("custom", "exact-model").unwrap();
    let description = model_with_optional_reasoning(model.clone());
    let remote = Arc::new(Remote {
        description: ConnectionDescription {
            wire_version: rsi_api_protocol::API_WIRE_VERSION,
            endpoint_id: EndpointId::from_bytes([1; 16]),
            host_epoch: HostEpoch::from_bytes([2; 16]),
        },
        operations: vec![
            rsi_settings_api::Operation::Read.spec(),
            rsi_ai_models_api::describe_operation(),
        ],
        result: Mutex::new(Some(Ok(json!(description)))),
        writes: 0.into(),
        discovery_failed: false.into(),
        reads: 0.into(),
    });
    let settings = Arc::new(DefaultSettings(Mutex::new(
        json!({"default_model":model,"default_reasoning_effort":"high"}),
    )));
    let runtime = rsi_meta::Runtime::default();
    let feature = Arc::new(SetupFeature {
        api: remote.clone(),
        authority: ConfigurationClient::new(remote.clone()),
        providers: ManagedProvidersClient::new(remote.clone()),
        credentials: ProviderCredentialsClient::new(remote.clone()),
        settings: settings.clone(),
        state: PresentationState::default(),
        previous_default: Default::default(),
        work: Work::new(runtime.execution().clone()),
    });
    feature.command(SetupCommand::Refresh).await.unwrap();
    assert_eq!(
        feature.snapshot().default_model_description,
        Some(description.clone())
    );
    assert_eq!(feature.view()["agent"]["default_reasoning_effort"], "high");
    assert_eq!(
        feature.view()["default_model_description"]["profile"]["reasoning_efforts"]["default"],
        "off"
    );
    assert_eq!(remote.writes.load(Ordering::SeqCst), 0);
    *settings.0.lock().unwrap() =
        json!({"default_model":{"deployment":"custom","model":"replacement"}});
    *remote.result.lock().unwrap() = Some(Ok(json!(description)));
    feature.command(SetupCommand::Refresh).await.unwrap();
    assert!(feature.snapshot().default_model_description.is_none());
    assert_eq!(
        feature.view()["agent"]["default_model"]["model"],
        "replacement"
    );
    *remote.result.lock().unwrap() = Some(Err(ApiError::Capacity));
    feature.command(SetupCommand::Refresh).await.unwrap();
    assert!(feature.snapshot().default_model_description.is_none());
    *settings.0.lock().unwrap() = json!({"sandbox":"read-only"});
    feature.command(SetupCommand::Refresh).await.unwrap();
    assert_eq!(remote.reads.load(Ordering::SeqCst), 3);
    assert!(feature.snapshot().default_model_description.is_none());
    let ticket = feature.snapshot().ticket;
    let select = || SetupCommand::DefaultModel {
        ticket: ticket.clone(),
        model: model.clone(),
        reasoning_effort: Some(ReasoningEffortId::new("high").unwrap()),
    };
    *remote.result.lock().unwrap() = Some(Ok(json!(description)));
    feature.command(select()).await.unwrap();
    assert_eq!(
        *settings.0.lock().unwrap(),
        json!({"default_model":model,"default_reasoning_effort":"high","sandbox":"read-only"})
    );
    assert!(feature.command(select()).await.is_err());
    feature.work.close().await;
    assert!(runtime.shutdown().await.is_clean());
}
#[tokio::test]
async fn confirmed_composer_selections_queue_in_order_and_keep_other_settings() {
    use rsi_agent_session_protocol::ModelSelection;
    use rsi_ai_protocol::{ModelRef, ReasoningEffortId};
    let remote = Arc::new(Remote {
        description: ConnectionDescription {
            wire_version: rsi_api_protocol::API_WIRE_VERSION,
            endpoint_id: EndpointId::from_bytes([1; 16]),
            host_epoch: HostEpoch::from_bytes([2; 16]),
        },
        operations: vec![
            rsi_settings_api::Operation::Read.spec(),
            rsi_settings_api::Operation::Replace.spec(),
        ],
        result: Mutex::new(None),
        writes: 0.into(),
        discovery_failed: false.into(),
        reads: 0.into(),
    });
    let settings = Arc::new(DefaultSettings(Mutex::new(json!({"extra":"retained"}))));
    let runtime = rsi_meta::Runtime::default();
    let feature = Arc::new(SetupFeature {
        api: remote.clone(),
        authority: ConfigurationClient::new(remote.clone()),
        providers: ManagedProvidersClient::new(remote.clone()),
        credentials: ProviderCredentialsClient::new(remote.clone()),
        settings: settings.clone(),
        state: PresentationState::default(),
        previous_default: Default::default(),
        work: Work::new(runtime.execution().clone()),
    });
    let first = ModelSelection {
        model: ModelRef::new("custom", "first").unwrap(),
        reasoning_effort: None,
    };
    let last = ModelSelection {
        model: ModelRef::new("custom", "last").unwrap(),
        reasoning_effort: Some(ReasoningEffortId::new("high").unwrap()),
    };
    let description = model_with_optional_reasoning(last.model.clone());
    feature.remember_model(&first, None);
    assert!(feature.previous_default.lock().unwrap().is_none());
    feature.command(SetupCommand::Refresh).await.unwrap();
    let busy = feature.work.slot.acquire().await.unwrap();
    feature.remember_model(&first, None);
    feature.remember_model(&last, Some(&description));
    feature.remember_model(&last, Some(&description));
    assert_eq!(*settings.0.lock().unwrap(), json!({"extra":"retained"}));
    drop(busy);
    feature.work.close().await;
    let view = feature.snapshot();
    assert_eq!(
        view.agent,
        json!({"extra":"retained","default_model":last.model,"default_reasoning_effort":"high"})
    );
    assert_eq!(view.agent, *settings.0.lock().unwrap());
    assert_eq!(view.default_model_description, Some(description));
    assert_eq!(view.receipts.len(), 2);
    assert_eq!(remote.reads.load(Ordering::SeqCst), 0);
    assert!(runtime.shutdown().await.is_clean());
}

#[derive(Debug, Default)]
struct NoSettings(AtomicUsize);
#[async_trait::async_trait]
impl SettingsAccess for NoSettings {
    async fn list(&self, _: Option<&str>, _: usize) -> rsi_settings_protocol::Result<SettingsPage> {
        unreachable!()
    }
    async fn describe(&self, _: &str) -> rsi_settings_protocol::Result<SettingsDescription> {
        unreachable!()
    }
    async fn read(&self, _: &str) -> rsi_settings_protocol::Result<SettingsSnapshot> {
        unreachable!()
    }
    async fn replace(
        &self,
        _: &str,
        _: &SettingsVersion,
        _: serde_json::Value,
    ) -> rsi_settings_protocol::Result<SettingsSnapshot> {
        self.0.fetch_add(1, Ordering::SeqCst);
        unreachable!("failed provider apply must not save a default")
    }
    async fn clear(
        &self,
        _: &str,
        _: &SettingsVersion,
    ) -> rsi_settings_protocol::Result<SettingsSnapshot> {
        unreachable!()
    }
}

#[tokio::test]
async fn failed_unknown_or_unapplied_provider_writes_never_save_a_default_or_replay() {
    let definition = ManagedProvider {
        provider: ProviderKind::Deepseek,
        config: serde_json::json!({"deployment":"new","language_models":{"model":{}}}),
    };
    let pending = serde_json::json!({"desired_revision":"1","applied_revision":"0","applying":false,"diagnostic":"route conflict","deployments":[definition]});
    let restart = serde_json::json!({"desired_revision":"1","applied_revision":"0","applying":false,"diagnostic":"Managed provider requires restart","deployments":[definition]});
    for (result, expected_receipt) in [
        (Ok(restart), "confirmed"),
        (Err(ApiError::OutcomeUnknown), "unknown"),
        (
            Err(ApiError::Invalid("revision conflict; refresh".into())),
            "failed",
        ),
        (Ok(serde_json::json!({"malformed":true})), "unknown"),
        (Ok(pending), "confirmed"),
    ] {
        let diagnostic = result
            .as_ref()
            .ok()
            .and_then(|value| value["diagnostic"].as_str())
            .map(str::to_owned);
        let remote = Arc::new(Remote {
            description: ConnectionDescription {
                wire_version: rsi_api_protocol::API_WIRE_VERSION,
                endpoint_id: EndpointId::from_bytes([1; 16]),
                host_epoch: HostEpoch::from_bytes([2; 16]),
            },
            operations: vec![
                ConfigurationOperation::Status.spec(),
                CredentialOperation::Status.spec(),
                CredentialOperation::Set.spec(),
                CredentialOperation::Unset.spec(),
                ProvidersOperation::Read.spec(),
                ProvidersOperation::Replace.spec(),
            ],
            result: Mutex::new(Some(result)),
            writes: AtomicUsize::new(0),
            discovery_failed: false.into(),
            reads: AtomicUsize::new(0),
        });
        let settings = Arc::new(NoSettings::default());
        let runtime = rsi_meta::Runtime::default();
        let feature = Arc::new(SetupFeature {
            api: remote.clone(),
            authority: ConfigurationClient::new(remote.clone()),
            providers: ManagedProvidersClient::new(remote.clone()),
            credentials: ProviderCredentialsClient::new(remote.clone()),
            settings: settings.clone(),
            state: crate::PresentationState::default(),
            previous_default: Default::default(),
            work: Work::new(runtime.execution().clone()),
        });
        let view = SetupView {
            ticket: "view".into(),
            providers: Some(ProvidersSnapshot {
                desired_revision: "0".into(),
                applied_revision: "0".into(),
                deployments: vec![],
                applying: false,
                diagnostic: None,
            }),
            ..SetupView::default()
        };
        reject_invalid_deployments(&feature, &view, &definition, &remote).await;
        let save = feature.save_model(
            view.clone(),
            definition.clone(),
            rsi_ai_protocol::ModelRef::new("new", "model").unwrap(),
            true,
        );
        // Admission is synchronous, so this caller races retained work even
        // before the first caller polls its receipt waiter.
        assert!(
            feature
                .save_model(
                    view,
                    definition.clone(),
                    rsi_ai_protocol::ModelRef::new("new", "model").unwrap(),
                    true
                )
                .await
                .unwrap_err()
                .contains("still running")
        );
        let failure = save.await.unwrap_err();
        if let Some(diagnostic) = diagnostic {
            assert!(
                failure.contains(&diagnostic),
                "save failure lost the Host's reason: {failure}"
            );
        }
        assert_eq!(
            feature.snapshot().diagnostic.as_deref(),
            Some(failure.as_str())
        );
        assert_eq!(remote.writes.load(Ordering::SeqCst), 1);
        assert_eq!(settings.0.load(Ordering::SeqCst), 0);
        assert_eq!(feature.snapshot().receipts[0].outcome, expected_receipt);
        feature.work.close().await;
        assert!(runtime.shutdown().await.is_clean());
    }
}

async fn reject_invalid_deployments(
    feature: &Arc<SetupFeature>,
    view: &SetupView,
    definition: &ManagedProvider,
    remote: &Remote,
) {
    for identity in [
        None,
        Some(Value::Null),
        Some(json!(3)),
        Some(json!("different")),
    ] {
        let mut malformed = definition.clone();
        if let Some(identity) = identity {
            malformed.config["deployment"] = identity;
        } else {
            malformed
                .config
                .as_object_mut()
                .unwrap()
                .remove("deployment");
        }
        assert!(
            feature
                .save_model(
                    view.clone(),
                    malformed,
                    rsi_ai_protocol::ModelRef::new("new", "model").unwrap(),
                    false
                )
                .await
                .is_err()
        );
        assert_eq!(
            remote.writes.load(Ordering::SeqCst),
            0,
            "invalid deployment must fail before provider publication"
        );
    }
}

#[tokio::test]
async fn setup_refresh_uses_last_catalog_when_discovery_fails() {
    let remote = Arc::new(Remote {
        description: ConnectionDescription {
            wire_version: rsi_api_protocol::API_WIRE_VERSION,
            endpoint_id: EndpointId::from_bytes([1; 16]),
            host_epoch: HostEpoch::from_bytes([2; 16]),
        },
        operations: vec![ConfigurationOperation::Status.spec()],
        result: Mutex::new(None),
        writes: AtomicUsize::new(0),
        discovery_failed: false.into(),
        reads: AtomicUsize::new(0),
    });
    let runtime = rsi_meta::Runtime::default();
    let feature = SetupFeature {
        api: remote.clone(),
        authority: ConfigurationClient::new(remote.clone()),
        providers: ManagedProvidersClient::new(remote.clone()),
        credentials: ProviderCredentialsClient::new(remote.clone()),
        settings: Arc::new(NoSettings::default()),
        state: crate::PresentationState::default(),
        previous_default: Default::default(),
        work: Work::new(runtime.execution().clone()),
    };
    remote.discovery_failed.store(true, Ordering::SeqCst);
    assert!(feature.refresh().await.is_err());
    assert_eq!(remote.reads.load(Ordering::SeqCst), 0);
    remote.discovery_failed.store(false, Ordering::SeqCst);
    feature.refresh().await.unwrap();
    let first_ticket = feature.snapshot().ticket;
    let (revision, view) = feature.snapshot_if_changed(None).unwrap();
    assert_eq!(view["ticket"], first_ticket);
    assert!(feature.snapshot_if_changed(Some(revision)).is_none());
    remote.discovery_failed.store(true, Ordering::SeqCst);
    feature.refresh().await.unwrap();
    assert_eq!(remote.reads.load(Ordering::SeqCst), 2);
    assert!(feature.snapshot().allowed);
    assert_ne!(feature.snapshot().ticket, first_ticket);
    let (next_revision, view) = feature.snapshot_if_changed(Some(revision)).unwrap();
    assert_ne!(next_revision, revision);
    assert_eq!(view["ticket"], feature.snapshot().ticket);
    assert!(feature.snapshot_if_changed(Some(next_revision)).is_none());
    assert!(!feature.snapshot().credentials_available);
    feature.work.close().await;
    assert!(runtime.shutdown().await.is_clean());
}
