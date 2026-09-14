use super::*;
use crate::Work;
use rsi_api_protocol::{
    ApiClient, ApiError, ApiMessage, ApiOutput, ByteBudget, ConnectionDescription, EndpointId,
    HostEpoch, OperationClass, OperationSpec, RetainedBytes,
};
use rsi_configuration_api::{ConfigurationOperation, CredentialOperation, ProvidersOperation};
use rsi_settings_protocol::{SettingsDescription, SettingsPage, SettingsVersion};
use std::sync::atomic::{AtomicUsize, Ordering};

#[derive(Debug)]
struct Remote {
    description: ConnectionDescription,
    operations: Vec<OperationSpec>,
    result: Mutex<Option<rsi_api_protocol::Result<serde_json::Value>>>,
    writes: AtomicUsize,
}
#[async_trait::async_trait]
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
        assert_eq!(operation, &ProvidersOperation::Replace.spec());
        self.writes.fetch_add(1, Ordering::SeqCst);
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
                wire_version: 1,
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
        });
        let settings = Arc::new(NoSettings::default());
        let runtime = rsi_meta::Runtime::default();
        let feature = Arc::new(SetupFeature {
            authority: ConfigurationClient::new(remote.clone()).unwrap(),
            providers: ManagedProvidersClient::new(remote.clone()).unwrap(),
            credentials: ProviderCredentialsClient::new(remote.clone()).unwrap(),
            settings: settings.clone(),
            state: Mutex::default(),
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
