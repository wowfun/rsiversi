//! Durable form-managed provider configurations, applied through an ordinary child Profile.
#![deny(unsafe_code)]
#![warn(missing_docs)]
#![allow(clippy::missing_errors_doc)]

use async_trait::async_trait;
use futures_util::future::BoxFuture;
use rsi_api_protocol::{ApiError, ApiRegistrarContract, CallOrigin, Result};
use rsi_application::ScopedProfile;
use rsi_configuration_access::{ConfigurationAccess, ConfigurationAccessContract};
use rsi_configuration_api::{ManagedProvider, ProviderKind, ProvidersSnapshot};
use rsi_host::{
    Host, HostBuilder, Profile, ProfileEntry, ProfileInput, ProfileProgram, ReloadOutcome,
};
use rsi_meta::{
    ActivationPlan, ConfigValue, Execution, LocalContract, MetaError, PluginFactory,
    PreparedActivation, UpdateMode,
};
use rsi_storage_domain::{Domain, DomainFacilityContract, DomainSpec};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeSet,
    sync::{Arc, Mutex},
};
use tokio::sync::Semaphore;
use tokio_util::task::TaskTracker;

mod endpoint;

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Document {
    revision: u64,
    deployments: Vec<ManagedProvider>,
}
#[derive(Debug)]
struct State {
    document: Document,
    applied: u64,
    applying: bool,
    diagnostic: Option<String>,
    uncertain: bool,
    closed: bool,
}

/// One Host-owned durable desired document and its child Profile convergence.
#[derive(Debug)]
pub struct ManagedProviders {
    domain: Arc<dyn Domain>,
    access: Arc<ConfigurationAccess>,
    host: Arc<Host>,
    profile: ScopedProfile,
    state: Mutex<State>,
    writer: Arc<Semaphore>,
    tasks: TaskTracker,
    execution: Execution,
}
impl ManagedProviders {
    /// Reads desired and applied state without claiming provider connectivity.
    ///
    /// # Panics
    /// Panics if an earlier panic poisoned this owner's state lock.
    pub fn snapshot(&self) -> ProvidersSnapshot {
        let state = self.state.lock().expect("managed providers state poisoned");
        ProvidersSnapshot {
            desired_revision: state.document.revision.to_string(),
            applied_revision: state.applied.to_string(),
            deployments: state.document.deployments.clone(),
            applying: state.applying,
            diagnostic: state.diagnostic.clone(),
        }
    }
    /// Admits one exact replacement; the owner retains execution if its waiter is lost.
    ///
    /// # Panics
    /// Panics if an earlier panic poisoned this owner's state lock.
    pub fn replace(
        self: &Arc<Self>,
        origin: &CallOrigin,
        expected: &str,
        deployments: Vec<ManagedProvider>,
    ) -> Result<BoxFuture<'static, Result<ProvidersSnapshot>>> {
        let expected = expected
            .parse::<u64>()
            .ok()
            .filter(|value| value.to_string() == expected)
            .ok_or_else(|| ApiError::Invalid("invalid provider revision".into()))?;
        let lease = self.access.admit(origin)?;
        let state = self.state.lock().expect("managed providers state poisoned");
        if state.closed {
            return Err(ApiError::ShuttingDown);
        }
        if state.uncertain {
            return Err(ApiError::OutcomeUnknown);
        }
        if state.document.revision != expected {
            return Err(ApiError::Invalid(
                "provider revision conflict; refresh configuration".into(),
            ));
        }
        let permit = self
            .writer
            .clone()
            .try_acquire_owned()
            .map_err(|_| ApiError::Capacity)?;
        let revision = expected
            .checked_add(1)
            .ok_or_else(|| ApiError::Invalid("provider revision exhausted".into()))?;
        let document = Document {
            revision,
            deployments,
        };
        validate_document(&document)?;
        let owner = self.clone();
        let task = self.execution.spawn(self.tasks.track_future(async move {
            let (_permit, _lease) = (permit, lease);
            let input = owner.preflight(&document).await?;
            if owner.domain.put("desired", serde_json::to_value(&document).map_err(|_| ApiError::Invalid("invalid provider document".into()))?).await.is_err() {
                let mut state = owner.state.lock().expect("managed providers state poisoned");
                state.uncertain = true;
                state.diagnostic = Some("Provider storage outcome is unknown. This is the last confirmed configuration; restart the Host to reload durable truth before editing.".into());
                return Err(ApiError::OutcomeUnknown);
            }
            {
                let mut state = owner.state.lock().expect("managed providers state poisoned");
                state.document = document; state.applying = true; state.diagnostic = None;
            }
            owner.converge(input).await;
            Ok(owner.snapshot())
        }));
        drop(state);
        Ok(Box::pin(async move {
            task.await.map_err(|_| ApiError::OutcomeUnknown)?
        }))
    }
    async fn preflight(&self, document: &Document) -> Result<ProfileInput> {
        let host = self.host.clone();
        let program = program(document);
        self.execution
            .prepare(move || {
                let input = host
                    .profile_input(program)
                    .map_err(|_| ApiError::Invalid("invalid managed provider profile".into()))?;
                input.preflight_linked(&BTreeSet::new()).map_err(|_| {
                    ApiError::Invalid("provider configuration rejected by its factory".into())
                })?;
                Ok(input)
            })
            .await
            .map_err(|_| ApiError::Unavailable)?
    }
    async fn converge(&self, input: ProfileInput) {
        let updater = self.profile.updater();
        let outcome = match updater.submit(updater.input_revision(), input) {
            Ok(ticket) => ticket.wait().await,
            Err(error) => Err(error),
        };
        let mut state = self.state.lock().expect("managed providers state poisoned");
        state.applying = false;
        match outcome {
            Ok(ReloadOutcome::Applied(_) | ReloadOutcome::Unchanged(_)) => {
                state.applied = state.document.revision;
                state.diagnostic = None;
            }
            Ok(ReloadOutcome::RestartRequired(_)) => {
                state.diagnostic = Some("Managed provider requires restart".into());
            }
            Ok(ReloadOutcome::RolledBack { .. }) => state.diagnostic = Some(
                "Provider activation failed; previous routes restored. Check deployment conflicts."
                    .into(),
            ),
            Ok(ReloadOutcome::Degraded { .. }) => state.diagnostic = Some(
                "Provider activation and recovery failed; inspect current Models before retrying."
                    .into(),
            ),
            Err(_) => {
                state.diagnostic = Some(
                    "Provider application did not complete; refresh before making another change."
                        .into(),
                );
            }
        }
    }
    async fn close(&self) -> rsi_meta::CleanupReport {
        {
            let mut state = self.state.lock().expect("managed providers state poisoned");
            state.closed = true;
            self.writer.close();
            self.tasks.close();
        }
        self.tasks.wait().await;
        self.profile.shutdown().await
    }
}
fn validate_document(document: &Document) -> Result<()> {
    if document.deployments.len() > 64
        || serde_json::to_vec(&serde_json::json!({"desired": document}))
            .map_err(|_| ApiError::Invalid("invalid provider document".into()))?
            .len()
            > 1024 * 1024
    {
        return Err(ApiError::Invalid(
            "managed providers exceed 64 deployments or 1 MiB".into(),
        ));
    }
    let mut names = BTreeSet::new();
    for deployment in &document.deployments {
        let name = deployment
            .config
            .get("deployment")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| ApiError::Invalid("provider deployment name is required".into()))?;
        if !names.insert(name) {
            return Err(ApiError::Invalid("duplicate managed deployment".into()));
        }
        if deployment
            .config
            .pointer("/credential/owner")
            .and_then(serde_json::Value::as_str)
            != Some(deployment.provider.owner())
        {
            return Err(ApiError::Invalid(
                "managed credentials must belong to the selected provider".into(),
            ));
        }
    }
    Ok(())
}
fn plugin(kind: ProviderKind) -> &'static str {
    kind.owner()
}
fn program(document: &Document) -> ProfileProgram {
    ProfileProgram::from_profile(Profile::new(document.deployments.iter().map(
        |definition| {
            ProfileEntry::new(
                format!(
                    "managed.{}",
                    definition.config["deployment"].as_str().unwrap_or_default()
                ),
                plugin(definition.provider),
                definition.config.clone(),
            )
        },
    )))
}
fn catalog(execution: Execution) -> rsi_host::Result<Host> {
    let mut builder = HostBuilder::without_paths(std::env::consts::OS).execution(execution);
    for (kind, factory) in [
        (
            ProviderKind::Openai,
            Arc::new(rsi_ai_openai::OpenAiFactory::default()) as Arc<dyn PluginFactory>,
        ),
        (
            ProviderKind::OpenaiCompatible,
            Arc::new(rsi_ai_openai_compatible::OpenAiCompatibleFactory::default()),
        ),
        (
            ProviderKind::Deepseek,
            Arc::new(rsi_ai_deepseek::DeepSeekFactory::default()),
        ),
    ] {
        builder.register_linked(
            plugin(kind),
            env!("CARGO_PKG_VERSION"),
            UpdateMode::Replayable,
            factory,
        )?;
    }
    builder.build()
}
/// Nominal management capability; it has no credential resolution authority.
#[derive(Debug)]
pub struct ManagedProvidersContract;
impl LocalContract for ManagedProvidersContract {
    const KEY: &'static str = "rsi.managed-providers";
    type Service = ManagedProviders;
}

/// Ordinary Host plugin with an explicit Storage backend.
#[derive(Clone, Debug, Default)]
pub struct ManagedProvidersFactory;
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Config {
    backend: String,
}
#[async_trait]
impl PluginFactory for ManagedProvidersFactory {
    fn prepare(&self, config: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        let input: Config = serde_json::from_value(config.clone()).map_err(|_| {
            MetaError::InvalidInput("invalid managed provider owner configuration".into())
        })?;
        if input.backend.is_empty() || input.backend.len() > 256 {
            return Err(MetaError::InvalidInput(
                "invalid managed provider backend".into(),
            ));
        }
        Ok(PreparedActivation::new(config.clone())
            .requiring_local::<DomainFacilityContract>()
            .requiring_local::<ConfigurationAccessContract>()
            .requiring_local::<rsi_host::ProfileControlContract>()
            .requiring_local::<ApiRegistrarContract>()
            .requiring_local::<rsi_ai_provider::LanguageRegistrarContract>()
            .requiring_local::<rsi_ai_provider::ImageRegistrarContract>())
    }
    async fn activate(&self, plan: ActivationPlan) -> rsi_meta::Result<()> {
        let input: Config = serde_json::from_value(plan.config().as_ref().clone())
            .map_err(|_| MetaError::Activation("invalid managed provider configuration".into()))?;
        let execution = plan.context().runtime().execution().clone();
        let host = Arc::new(catalog(execution.clone()).map_err(activation)?);
        let domain = plan
            .local::<DomainFacilityContract>()?
            .open(DomainSpec {
                id: "rsi.managed-providers".into(),
                backend: input.backend,
                version: 1,
                maximum_records: 1,
                maximum_bytes: 1024 * 1024,
            })
            .await
            .map_err(activation)?;
        let mut records = domain.snapshot().await;
        if records.len() > 1 || records.keys().any(|key| key != "desired") {
            return Err(MetaError::Activation(
                "invalid managed provider records".into(),
            ));
        }
        let document: Document = records
            .remove("desired")
            .map_or_else(|| Ok(Document::default()), serde_json::from_value)
            .map_err(|_| MetaError::Activation("invalid managed provider document".into()))?;
        validate_document(&document).map_err(activation)?;
        // Preflight durable inputs before even the empty child scope is activated.
        let capture = host.clone();
        let desired = program(&document);
        let prepared = execution
            .prepare(move || {
                let input = capture.profile_input(desired)?;
                input.preflight_linked(&BTreeSet::new())?;
                Ok::<_, rsi_host::HostError>(input)
            })
            .await
            .map_err(activation)?
            .map_err(activation)?;
        let profile = ScopedProfile::start(&host, plan.context(), program(&Document::default()))
            .await
            .map_err(activation)?;
        let owner = Arc::new(ManagedProviders {
            domain,
            access: plan.local::<ConfigurationAccessContract>()?,
            host,
            profile,
            state: Mutex::new(State {
                document,
                applied: 0,
                applying: true,
                diagnostic: None,
                uncertain: false,
                closed: false,
            }),
            writer: Arc::new(Semaphore::new(1)),
            tasks: TaskTracker::new(),
            execution,
        });
        owner.converge(prepared).await;
        // Register cleanup before fallible publication so partial activation also drains.
        let closing = owner.clone();
        plan.defer(
            "drain managed providers",
            Box::new(move || {
                Box::pin(async move {
                    let report = closing.close().await;
                    if report.is_clean() {
                        Ok(())
                    } else {
                        Err("managed provider cleanup failed".into())
                    }
                })
            }),
        )?;
        let registrations = endpoint::register(
            plan.local::<ApiRegistrarContract>()?.as_ref(),
            owner.clone(),
        )
        .map_err(activation)?;
        let supply = plan
            .context()
            .provide_local::<ManagedProvidersContract>(owner)?;
        plan.defer(
            "close managed provider API",
            Box::new(move || {
                Box::pin(async move {
                    drop(supply);
                    futures_util::future::join_all(
                        registrations
                            .into_iter()
                            .map(rsi_api_protocol::ApiRegistration::close),
                    )
                    .await;
                    Ok(())
                })
            }),
        )
    }
}
fn activation(error: impl std::fmt::Display) -> MetaError {
    MetaError::Activation(error.to_string())
}
