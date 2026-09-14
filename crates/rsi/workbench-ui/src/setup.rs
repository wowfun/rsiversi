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
        /// Requested default effort; absence resets to this model's provider default.
        reasoning_effort: Option<rsi_ai_protocol::ReasoningEffortId>,
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
/// Credential availability without the credential value.
#[derive(Clone, Debug, Serialize)]
pub struct CredentialView {
    /// Provider family.
    pub provider: ProviderKind,
    /// Provider-owned credential slot.
    pub slot: String,
    /// Redacted credential availability.
    pub status: CredentialStatus,
}
/// One independent setup operation result.
#[derive(Clone, Debug, Serialize)]
pub struct Receipt {
    /// Independent operation name.
    pub operation: String,
    /// Confirmed, failed, or unknown result.
    pub outcome: String,
    /// Bounded redacted receipt detail.
    pub message: String,
}
/// Typed redacted application setup projection.
#[derive(Clone, Debug, Default, Serialize)]
pub struct SetupView {
    /// Exact version of this setup view.
    pub ticket: String,
    /// Whether configuration mutations are authorized.
    pub allowed: bool,
    /// Desired and applied provider configuration.
    pub providers: Option<ProvidersSnapshot>,
    /// Current non-secret Agent defaults.
    pub agent: Value,
    /// Current preset selection and roots.
    pub presets: Value,
    /// Most recently inspected credential status.
    pub credential: Option<CredentialView>,
    /// Recent independent operation receipts.
    pub receipts: Vec<Receipt>,
    /// Most recent bounded operation failure.
    pub diagnostic: Option<String>,
}
#[derive(Debug, Default)]
struct State {
    view: SetupView,
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
    /// Captures typed redacted state for native application clients.
    ///
    /// # Panics
    /// Panics if an earlier panic poisoned the setup state lock.
    pub fn snapshot(&self) -> SetupView {
        self.state.lock().expect("setup view poisoned").view.clone()
    }
    /// Explicit cancellable discovery; dropping this read discards its result.
    pub async fn discover(
        &self,
        request: rsi_configuration_api::DiscoveryRequest,
    ) -> Result<rsi_configuration_api::DiscoverySnapshot> {
        self.providers.discover(request).await.map_err(error)
    }
    /// Saves a model against the displayed provider revision, then optionally the default.
    /// Each step retains its own receipt; failed or uncertain mutations are never replayed.
    ///
    /// # Panics
    /// Panics if an earlier panic poisoned the setup state lock.
    pub fn save_model(
        self: &Arc<Self>,
        view: SetupView,
        definition: ManagedProvider,
        model: rsi_ai_protocol::ModelRef,
        default: bool,
    ) -> BoxFuture<'static, Result<()>> {
        let owner = self.clone();
        self.run(async move {
            let providers = view.providers.ok_or("Refresh setup before saving")?;
            let deployment = definition
                .config
                .get("deployment")
                .and_then(Value::as_str)
                .filter(|deployment| *deployment == model.deployment())
                .ok_or("Provider deployment must match the selected model before saving")?;
            let mut deployments = providers.deployments;
            let existing = deployments.iter().position(|entry| {
                entry.config.get("deployment").and_then(Value::as_str) == Some(deployment)
            });
            if let Some(index) = existing {
                deployments[index] = definition;
            } else {
                deployments.push(definition);
            }
            owner
                .execute(SetupCommand::ProvidersReplace {
                    expected_revision: providers.desired_revision,
                    deployments,
                })
                .await?;
            {
                let state = owner.state.lock().expect("setup view poisoned");
                let applied = state
                    .view
                    .providers
                    .as_ref()
                    .ok_or("Provider status unavailable")?;
                if applied.applying
                    || applied.applied_revision != applied.desired_revision
                    || applied.diagnostic.is_some()
                {
                    let reason = applied
                        .diagnostic
                        .as_deref()
                        .unwrap_or("Refresh provider status before selecting this model.");
                    return Err(format!(
                        "Provider configuration saved, but routes are not applied. {reason}"
                    ));
                }
            }
            if default {
                owner
                    .execute(SetupCommand::DefaultModel {
                        ticket: view.ticket,
                        model,
                        reasoning_effort: None,
                    })
                    .await?;
            }
            Ok(())
        })
    }

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
        self.run(async move { owner.execute(command).await })
    }
    fn run(
        self: &Arc<Self>,
        future: impl std::future::Future<Output = Result<()>> + Send + 'static,
    ) -> BoxFuture<'static, Result<()>> {
        let owner = self.clone();
        self.work.run(async move {
            let result = future.await;
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
            SetupCommand::DefaultModel {
                ticket,
                model,
                reasoning_effort,
            } => {
                self.select(
                    &ticket,
                    "rsi.agent",
                    "default_model",
                    serde_json::to_value(model).map_err(error)?,
                    Some((
                        "default_reasoning_effort",
                        serde_json::to_value(reasoning_effort).map_err(error)?,
                    )),
                )
                .await
            }
            SetupCommand::DefaultPreset { ticket, preset } => {
                self.select(&ticket, "rsi.agent-presets", "default", json!(preset), None)
                    .await
            }
        }
    }
    async fn select(
        &self,
        ticket: &str,
        namespace: &str,
        field: &str,
        value: Value,
        extra: Option<(&str, Value)>,
    ) -> Result<()> {
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
        if let Some((key, value)) = extra {
            replacement[key] = value;
        }
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

#[cfg(test)]
#[path = "setup_tests.rs"]
mod tests;
