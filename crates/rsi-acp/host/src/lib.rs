//! Ordinary Host ownership of explicitly configured external ACP agents.
#![deny(unsafe_code)]
#![warn(missing_docs)]
#![allow(clippy::missing_errors_doc)]
mod config;
mod launch;
mod lifecycle;
mod service;
use async_trait::async_trait;
use config::{Config, EndpointConfig};
use rsi_acp_client::Handle;
use rsi_acp_journal::{ConversationId, Journal, Snapshot};
use rsi_acp_protocol::service::{Error, ExternalConversationsContract, Result};
use rsi_credentials_protocol::{CredentialsResolve, CredentialsResolveContract};
use rsi_meta::{ActivationPlan, ConfigValue, MetaError, PluginFactory, PreparedActivation};
use rsi_process::{DuplexProcess, DuplexProcessContract};
use rsi_sandbox::{Sandbox, SandboxContract};
use std::{
    collections::BTreeMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
};
use tokio::sync::watch;
use tokio_util::{sync::CancellationToken, task::TaskTracker};

struct Resident {
    endpoint: String,
    stop: CancellationToken,
    done: watch::Sender<Option<Result<Snapshot>>>,
    handle: Option<Handle>,
}
struct State {
    endpoints: Vec<EndpointConfig>,
    journal: Journal,
    credentials: Arc<dyn CredentialsResolve>,
    processes: Arc<dyn DuplexProcess>,
    sandbox: Arc<dyn Sandbox>,
    residents: Mutex<BTreeMap<ConversationId, Resident>>,
    stop: CancellationToken,
    tasks: TaskTracker,
    failed_cleanup: AtomicBool,
}
/// Host-retained service owner. Its Debug representation contains no launch inputs.
pub struct Owner {
    state: Arc<State>,
}
impl std::fmt::Debug for Owner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ExternalConversationsOwner")
            .finish_non_exhaustive()
    }
}
impl Owner {
    /// Stops admission and awaits all preparing, active and retiring peer owners.
    ///
    /// # Panics
    /// Panics if a prior internal panic poisoned resident admission.
    pub async fn shutdown(&self) -> Result<()> {
        {
            let _admission = self.state.residents.lock().expect("ACP residents");
            self.state.stop.cancel();
            self.state.tasks.close();
        }
        self.state.tasks.wait().await;
        self.state.journal.close().await.map_err(journal)?;
        if self.state.failed_cleanup.load(Ordering::Acquire) {
            Err(Error::Unknown)
        } else {
            Ok(())
        }
    }
}
impl Drop for Owner {
    fn drop(&mut self) {
        self.state.stop.cancel();
    }
}
impl State {
    fn handle(&self, id: &ConversationId) -> Result<Handle> {
        if self.stop.is_cancelled() {
            return Err(Error::Unknown);
        }
        self.residents
            .lock()
            .expect("ACP residents")
            .get(id)
            .and_then(|resident| resident.handle.clone())
            .ok_or(Error::NotFound)
    }
}
fn journal(error: rsi_acp_journal::Error) -> Error {
    match error {
        rsi_acp_journal::Error::NotFound => Error::NotFound,
        rsi_acp_journal::Error::Busy => Error::Busy,
        rsi_acp_journal::Error::Stale => Error::Stale,
        rsi_acp_journal::Error::Input => Error::Input,
        _ => Error::Journal,
    }
}
fn client(error: rsi_acp_client::Error) -> Error {
    match error {
        rsi_acp_client::Error::Input => Error::Input,
        rsi_acp_client::Error::Busy => Error::Busy,
        rsi_acp_client::Error::Stale => Error::Stale,
        rsi_acp_client::Error::Unsupported => Error::Unsupported,
        rsi_acp_client::Error::Remote => Error::Remote,
        rsi_acp_client::Error::Unknown => Error::Unknown,
        rsi_acp_client::Error::Journal => Error::Journal,
    }
}
fn meta() -> MetaError {
    MetaError::Activation("External ACP configuration or storage unavailable".into())
}
/// Ordinary endpoint owner. Configuration is Local Profile input only.
#[derive(Debug, Default)]
pub struct Factory;
#[async_trait]
impl PluginFactory for Factory {
    fn prepare(&self, value: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        let config: Config = serde_json::from_value(value.clone()).map_err(|_| meta())?;
        config.validate().map_err(|_| meta())?;
        Ok(PreparedActivation::new(value.clone())
            .requiring_local::<CredentialsResolveContract>()
            .requiring_local::<DuplexProcessContract>()
            .requiring_local::<SandboxContract>())
    }
    async fn activate(&self, plan: ActivationPlan) -> rsi_meta::Result<()> {
        let config: Config =
            serde_json::from_value(plan.config().as_ref().clone()).map_err(|_| meta())?;
        let owner = Arc::new(Owner {
            state: Arc::new(State {
                endpoints: config.endpoints,
                journal: Journal::open(config.directory, rsi_acp_journal::Limits::default())
                    .await
                    .map_err(|_| meta())?,
                credentials: plan.local::<CredentialsResolveContract>()?,
                processes: plan.local::<DuplexProcessContract>()?,
                sandbox: plan.local::<SandboxContract>()?,
                residents: Mutex::new(BTreeMap::new()),
                stop: CancellationToken::new(),
                tasks: TaskTracker::new(),
                failed_cleanup: AtomicBool::new(false),
            }),
        });
        let cleanup = owner.clone();
        plan.defer(
            "retire external ACP peers",
            Box::new(move || {
                Box::pin(async move { cleanup.shutdown().await.map_err(|error| error.to_string()) })
            }),
        )?;
        plan.context()
            .provide_local::<ExternalConversationsContract>(owner)?;
        Ok(())
    }
}
