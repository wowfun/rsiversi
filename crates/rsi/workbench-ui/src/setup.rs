use super::{
    ActivationPlan, Arc, BoxFuture, ConfigValue, LocalContract, Mutex, PluginFactory,
    PreparedActivation, Result, Work, contribute, error, meta, prepare, watch,
};
use async_trait::async_trait;
use rsi_configuration_api::{
    ConfigurationClient, ManagedProvider, ManagedProvidersClient, ProviderCredentialsClient,
    ProviderKind, ProvidersSnapshot,
};
use rsi_credentials_protocol::{CredentialStatus, SecretValue};
use rsi_settings_protocol::{SettingsAccess, SettingsAccessContract, SettingsSnapshot};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// Closed setup form commands. Secret values never enter a serialized view.
#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum SetupCommand {
    /// Refresh current authority, provider convergence and default selections.
    Refresh,
    /// Inspect one provider-owned credential slot.
    CredentialStatus {
        /// Provider family.
        provider: ProviderKind,
        /// Exact owner-local slot.
        slot: String,
    },
    /// Set one explicitly supplied credential, independently of all other settings.
    CredentialSet {
        /// Provider family.
        provider: ProviderKind,
        /// Exact owner-local slot.
        slot: String,
        /// Ephemeral zeroizing secret.
        #[serde(deserialize_with = "secret")]
        secret: SecretValue,
    },
    /// Delete one credential with its own receipt.
    CredentialUnset {
        /// Provider family.
        provider: ProviderKind,
        /// Exact owner-local slot.
        slot: String,
    },
    /// Preflight and replace managed providers against the displayed revision.
    ProvidersReplace {
        /// Exact desired revision.
        expected_revision: String,
        /// Closed provider definitions.
        deployments: Vec<ManagedProvider>,
    },
    /// Select the model for future drafts using the retained Settings snapshot.
    DefaultModel {
        /// Exact setup view ticket.
        ticket: String,
        /// Selected provider route.
        model: rsi_ai_protocol::ModelRef,
    },
    /// Select the existing default Agent preset while preserving source roots.
    DefaultPreset {
        /// Exact setup view ticket.
        ticket: String,
        /// Existing preset identity.
        preset: rsi_agent_session_protocol::AgentPresetId,
    },
}
fn secret<'de, D: serde::Deserializer<'de>>(
    decoder: D,
) -> std::result::Result<SecretValue, D::Error> {
    SecretValue::new(String::deserialize(decoder)?).map_err(serde::de::Error::custom)
}
#[derive(Debug, Serialize)]
struct CredentialView {
    provider: ProviderKind,
    slot: String,
    status: CredentialStatus,
}
#[derive(Debug, Serialize)]
struct Receipt {
    operation: String,
    outcome: String,
    message: String,
}
#[derive(Debug, Default, Serialize)]
struct View {
    ticket: String,
    allowed: bool,
    providers: Option<ProvidersSnapshot>,
    agent: Value,
    presets: Value,
    credential: Option<CredentialView>,
    receipts: Vec<Receipt>,
    diagnostic: Option<String>,
}
#[derive(Debug, Default)]
struct State {
    view: View,
    agent: Option<SettingsSnapshot>,
    presets: Option<SettingsSnapshot>,
}
/// Setup presentation state owned by one ordinary application plugin.
#[derive(Debug)]
pub struct SetupFeature {
    authority: ConfigurationClient,
    providers: ManagedProvidersClient,
    credentials: ProviderCredentialsClient,
    settings: Arc<dyn SettingsAccess>,
    state: Mutex<State>,
    work: Work,
}
impl SetupFeature {
    /// Captures redacted form state; no credential value can be serialized here.
    ///
    /// # Panics
    /// Panics if an earlier panic poisoned this owner's state lock.
    pub fn view(&self) -> Value {
        serde_json::to_value(&self.state.lock().expect("setup view poisoned").view)
            .expect("setup view encoding")
    }
    /// Subscribes to complete setup projection changes.
    pub fn changes(&self) -> watch::Receiver<u64> {
        self.work.changed.subscribe()
    }
    /// Admits one retained setup action with a separate operation receipt.
    ///
    /// # Panics
    /// Panics if an earlier panic poisoned this owner's state lock.
    pub fn command(self: &Arc<Self>, command: SetupCommand) -> BoxFuture<'static, Result<()>> {
        let owner = self.clone();
        self.work.run(async move {
            let result = owner.execute(command).await;
            owner
                .state
                .lock()
                .expect("setup view poisoned")
                .view
                .diagnostic = result.as_ref().err().cloned();
            result
        })
    }
    fn receipt(
        &self,
        operation: &str,
        result: &std::result::Result<impl Sized, rsi_api_protocol::ApiError>,
    ) {
        let (outcome, message) = match result {
            Ok(_) => ("confirmed", "Operation completed. Other setup steps remain independent.".into()),
            Err(rsi_api_protocol::ApiError::OutcomeUnknown) => ("unknown", "Outcome unknown. Refresh status before making another change; this request was not replayed.".into()),
            Err(failure) => ("failed", error(failure)),
        };
        let mut state = self.state.lock().expect("setup view poisoned");
        if state.view.receipts.len() == 8 {
            state.view.receipts.remove(0);
        }
        state.view.receipts.push(Receipt {
            operation: operation.into(),
            outcome: outcome.into(),
            message,
        });
    }
    async fn refresh(&self) -> Result<()> {
        let allowed = self.authority.allowed().await.map_err(error)?;
        let providers = self.providers.read().await.map_err(error)?;
        let agent = self.settings.read("rsi.agent").await.map_err(error)?;
        let presets = self
            .settings
            .read("rsi.agent-presets")
            .await
            .map_err(error)?;
        let mut state = self.state.lock().expect("setup view poisoned");
        state.view.ticket = rsi_ui::fresh_identity("setup")?;
        state.view.allowed = allowed;
        state.view.providers = Some(providers);
        state.view.agent = agent.value.clone();
        state.view.presets = presets.value.clone();
        state.agent = Some(agent);
        state.presets = Some(presets);
        Ok(())
    }
    async fn credential_status(&self, provider: ProviderKind, slot: String) -> Result<()> {
        let status = self
            .credentials
            .status(provider, &slot)
            .await
            .map_err(error)?;
        self.state
            .lock()
            .expect("setup view poisoned")
            .view
            .credential = Some(CredentialView {
            provider,
            slot,
            status,
        });
        Ok(())
    }
    async fn execute(&self, command: SetupCommand) -> Result<()> {
        match command {
            SetupCommand::Refresh => self.refresh().await,
            SetupCommand::CredentialStatus { provider, slot } => {
                self.credential_status(provider, slot).await
            }
            SetupCommand::CredentialSet {
                provider,
                slot,
                secret,
            } => {
                let result = self.credentials.set(provider, &slot, secret).await;
                self.receipt("credential-set", &result);
                result.map_err(error)?;
                self.credential_status(provider, slot).await
            }
            SetupCommand::CredentialUnset { provider, slot } => {
                let result = self.credentials.unset(provider, &slot).await;
                self.receipt("credential-unset", &result);
                result.map_err(error)?;
                self.credential_status(provider, slot).await
            }
            SetupCommand::ProvidersReplace {
                expected_revision,
                deployments,
            } => {
                let result = self
                    .providers
                    .replace(&expected_revision, deployments)
                    .await;
                self.receipt("providers-apply", &result);
                self.state
                    .lock()
                    .expect("setup view poisoned")
                    .view
                    .providers = Some(result.map_err(error)?);
                Ok(())
            }
            SetupCommand::DefaultModel { ticket, model } => {
                self.select(
                    &ticket,
                    "rsi.agent",
                    "default_model",
                    serde_json::to_value(model).map_err(error)?,
                )
                .await
            }
            SetupCommand::DefaultPreset { ticket, preset } => {
                self.select(&ticket, "rsi.agent-presets", "default", json!(preset))
                    .await
            }
        }
    }
    async fn select(&self, ticket: &str, namespace: &str, field: &str, value: Value) -> Result<()> {
        let snapshot = {
            let state = self.state.lock().expect("setup view poisoned");
            if state.view.ticket != ticket {
                return Err("Setup changed; refresh before selecting a default".into());
            }
            if namespace == "rsi.agent" {
                state.agent.clone()
            } else {
                state.presets.clone()
            }
            .ok_or("Setup has not been loaded")?
        };
        let mut replacement = snapshot.value.clone();
        replacement[field] = value;
        let result = self
            .settings
            .replace(namespace, &snapshot.version(), replacement)
            .await;
        let receipt_result = result
            .as_ref()
            .map(|_| ())
            .map_err(|failure| match failure {
                rsi_settings_protocol::SettingsError::Api(error) => error.clone(),
                _ => rsi_api_protocol::ApiError::Invalid(error(failure)),
            });
        self.receipt(field, &receipt_result);
        result.map_err(error)?;
        self.refresh().await
    }
}
/// Nominal setup presentation capability.
#[derive(Debug)]
pub struct SetupFeatureContract;
impl LocalContract for SetupFeatureContract {
    const KEY: &'static str = "rsi.workbench.setup";
    type Service = SetupFeature;
}
/// Ordinary setup feature plugin over already-negotiated configuration APIs.
#[derive(Clone, Debug, Default)]
pub struct SetupFeatureFactory;
#[async_trait]
impl PluginFactory for SetupFeatureFactory {
    fn prepare(&self, config: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        Ok(prepare(config)?.requiring_local::<SettingsAccessContract>())
    }
    async fn activate(&self, plan: ActivationPlan) -> rsi_meta::Result<()> {
        let api = plan.local::<rsi_api_protocol::ApiClientContract>()?;
        let feature = Arc::new(SetupFeature {
            authority: ConfigurationClient::new(api.clone()).map_err(meta)?,
            providers: ManagedProvidersClient::new(api.clone()).map_err(meta)?,
            credentials: ProviderCredentialsClient::new(api).map_err(meta)?,
            settings: plan.local::<SettingsAccessContract>()?,
            state: Mutex::default(),
            work: Work::new(plan.context().runtime().execution().clone()),
        });
        let _initial = feature.command(SetupCommand::Refresh).await;
        let supply = plan
            .context()
            .provide_local::<SetupFeatureContract>(feature.clone())?;
        plan.defer(
            "drain setup feature",
            Box::new(move || {
                Box::pin(async move {
                    drop(supply);
                    feature.work.close().await;
                    Ok(())
                })
            }),
        )?;
        contribute(&plan, "rsi.workbench.setup", "Model setup", Arc::new(Card))
    }
}
#[derive(Debug)]
struct Card;
impl rsi_ui::SurfaceRenderer for Card {
    fn render(&self, target: &rsi_meta::Context) -> rsi_ui::Result<rsi_ui::UiView> {
        let feature = target
            .lookup_local::<SetupFeatureContract>()
            .ok_or(rsi_ui::UiError::Retired)?;
        let state = feature.state.lock().expect("setup view poisoned");
        Ok(rsi_ui::UiView {
            title: "Model setup".into(),
            elements: vec![
                rsi_ui::UiElement::Field {
                    label: "Configuration access".into(),
                    value: if state.view.allowed {
                        "Allowed"
                    } else {
                        "Read only"
                    }
                    .into(),
                },
                rsi_ui::UiElement::Field {
                    label: "Default model".into(),
                    value: state.view.agent.get("default_model").map_or_else(
                        || "Choose a model to start a conversation".into(),
                        ToString::to_string,
                    ),
                },
            ],
        })
    }
}
