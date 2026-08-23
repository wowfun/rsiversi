use super::{LOADER_CONTRACT_ID, LOADER_CONTRACT_VERSION, LOADER_SERVICE_KEY, NativeCatalog};
use async_trait::async_trait;
use futures_util::{StreamExt as _, TryStreamExt as _, stream};
use rsi_meta::{
    ConfigValue, Context, FactoryIdentity, FiberHandle, FiberSnapshot, MetaError, PluginDescriptor,
    PluginFactory, ProviderChannel, Provision, Result, ServiceEndpoint, ServiceFrame,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use tokio::sync::oneshot;

const MAX_CONCURRENT_PREFLIGHTS: usize = 8;
const MAX_LOADER_ENTRIES: usize = 1024;
const MAX_LOADER_ID_BYTES: usize = 128;

fn valid_loader_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= MAX_LOADER_ID_BYTES
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct LoaderConfig {
    #[serde(default)]
    pub entries: Vec<LoaderEntry>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct LoaderEntry {
    pub id: String,
    pub artifact: PathBuf,
    #[serde(default)]
    pub config: Value,
}

#[derive(Clone, Debug)]
pub struct LoaderFactory {
    catalog: NativeCatalog,
    descriptor: PluginDescriptor,
}

impl LoaderFactory {
    pub fn new(catalog: NativeCatalog) -> Self {
        Self {
            catalog,
            descriptor: PluginDescriptor::new(FactoryIdentity::builtin("rsi.meta.loader", "1"))
                .providing(Provision::new(
                    LOADER_SERVICE_KEY,
                    LOADER_CONTRACT_ID,
                    LOADER_CONTRACT_VERSION,
                )),
        }
    }
}

#[async_trait]
impl PluginFactory for LoaderFactory {
    fn descriptor(&self) -> &PluginDescriptor {
        &self.descriptor
    }

    fn validate_config(&self, config: ConfigValue) -> Result<ConfigValue> {
        let parsed: LoaderConfig = serde_json::from_value(config)
            .map_err(|error| MetaError::InvalidInput(error.to_string()))?;
        if parsed.entries.len() > MAX_LOADER_ENTRIES {
            return Err(MetaError::InvalidInput(format!(
                "loader accepts at most {MAX_LOADER_ENTRIES} entries"
            )));
        }
        let mut ids = BTreeSet::new();
        if parsed
            .entries
            .iter()
            .any(|entry| !valid_loader_id(&entry.id) || !ids.insert(entry.id.clone()))
        {
            return Err(MetaError::InvalidInput(format!(
                "loader entry ids must be unique and contain 1..={MAX_LOADER_ID_BYTES} bytes"
            )));
        }
        serde_json::to_value(parsed).map_err(|error| MetaError::InvalidInput(error.to_string()))
    }

    async fn activate(&self, context: Context, config: ConfigValue) -> Result<()> {
        let parsed: LoaderConfig = serde_json::from_value(config)
            .map_err(|error| MetaError::InvalidInput(error.to_string()))?;
        let catalog = self.catalog.clone();
        let runtime = context.runtime().clone();
        let prepared = stream::iter(parsed.entries)
            .map(move |entry| {
                let catalog = catalog.clone();
                let runtime = runtime.clone();
                async move {
                    let factory = tokio::task::spawn_blocking(move || catalog.load(entry.artifact))
                        .await
                        .map_err(|error| MetaError::Activation(error.to_string()))?
                        .map_err(|error| MetaError::Activation(error.to_string()))?;
                    let plugin =
                        tokio::task::spawn_blocking(move || runtime.prepare(factory, entry.config))
                            .await
                            .map_err(|error| MetaError::Activation(error.to_string()))??;
                    Ok::<_, MetaError>((entry.id, plugin))
                }
            })
            .buffered(MAX_CONCURRENT_PREFLIGHTS)
            .try_collect::<Vec<_>>()
            .await?;

        let state = Arc::new(Mutex::new(LoaderState::default()));
        for (id, plugin) in prepared {
            let handle = context.apply_prepared(plugin).await?;
            let mut entries = state.lock().expect("loader state poisoned");
            entries.claimed.insert(id.clone());
            entries.handles.insert(id, handle);
        }
        context.provide(
            LOADER_SERVICE_KEY,
            LOADER_CONTRACT_ID,
            LOADER_CONTRACT_VERSION,
            Arc::new(LoaderEndpoint {
                context: context.clone(),
                catalog: self.catalog.clone(),
                state,
            }),
        )
    }
}

#[derive(Debug, Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
enum LoaderCommand {
    Load(LoaderEntry),
    Reconfigure { id: String, config: Value },
    Unload { id: String },
    Inspect,
}

#[derive(Debug, Serialize)]
struct LoaderResponse {
    ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    fiber: Option<FiberSnapshot>,
    #[serde(skip_serializing_if = "Option::is_none")]
    fibers: Option<BTreeMap<String, FiberSnapshot>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

struct CommandResult {
    response: LoaderResponse,
    delivered: Option<oneshot::Sender<()>>,
}

impl CommandResult {
    fn immediate(response: LoaderResponse) -> Self {
        Self {
            response,
            delivered: None,
        }
    }
}

struct LoaderEndpoint {
    context: Context,
    catalog: NativeCatalog,
    state: Arc<Mutex<LoaderState>>,
}

#[derive(Default)]
struct LoaderState {
    handles: BTreeMap<String, FiberHandle>,
    claimed: BTreeSet<String>,
}

struct IdReservation {
    state: Arc<Mutex<LoaderState>>,
    id: Option<String>,
}

impl IdReservation {
    fn claim(state: &Arc<Mutex<LoaderState>>, id: String) -> Option<Self> {
        if !state
            .lock()
            .expect("loader state poisoned")
            .claimed
            .insert(id.clone())
        {
            return None;
        }
        Some(Self {
            state: Arc::clone(state),
            id: Some(id),
        })
    }

    fn publish(mut self, handle: FiberHandle) -> String {
        let id = self.id.take().expect("unpublished reservation");
        self.state
            .lock()
            .expect("loader state poisoned")
            .handles
            .insert(id.clone(), handle);
        id
    }

    fn retain_until_released(state: &Arc<Mutex<LoaderState>>, id: String) -> Self {
        debug_assert!(
            state
                .lock()
                .expect("loader state poisoned")
                .claimed
                .contains(&id),
            "an active Loader ID remains claimed"
        );
        Self {
            state: Arc::clone(state),
            id: Some(id),
        }
    }
}

impl Drop for IdReservation {
    fn drop(&mut self) {
        if let Some(id) = self.id.take() {
            self.state
                .lock()
                .expect("loader state poisoned")
                .claimed
                .remove(&id);
        }
    }
}

impl fmt::Debug for LoaderEndpoint {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("LoaderEndpoint")
            .finish_non_exhaustive()
    }
}

#[async_trait]
impl ServiceEndpoint for LoaderEndpoint {
    async fn serve(
        &self,
        _: rsi_meta::InvocationContext,
        mut channel: ProviderChannel,
    ) -> Result<()> {
        let cancellation = channel.cancellation();
        while let Some(frame) = channel.recv().await {
            let mut result = match serde_json::from_slice::<LoaderCommand>(frame.as_bytes()) {
                Ok(command) => {
                    tokio::select! {
                        biased;
                        () = cancellation.cancelled() => return Ok(()),
                        response = self.execute(command) => response,
                    }
                }
                Err(error) => CommandResult::immediate(LoaderResponse::error(error)),
            };
            let bytes = serde_json::to_vec(&result.response)
                .map_err(|error| MetaError::Service(error.to_string()))?;
            channel.send(ServiceFrame::new(bytes)).await?;
            if let Some(delivered) = result.delivered.take() {
                let _ = delivered.send(());
            }
        }
        Ok(())
    }
}

impl LoaderEndpoint {
    async fn execute(&self, command: LoaderCommand) -> CommandResult {
        match command {
            LoaderCommand::Load(entry) => {
                if !valid_loader_id(&entry.id) {
                    return CommandResult::immediate(LoaderResponse::error(
                        "invalid loader entry id",
                    ));
                }
                let Some(reservation) = IdReservation::claim(&self.state, entry.id.clone()) else {
                    return CommandResult::immediate(LoaderResponse::error(
                        "loader entry id already exists",
                    ));
                };
                let context = self.context.clone();
                let catalog = self.catalog.clone();
                let state = Arc::clone(&self.state);
                let (response, receiver) = oneshot::channel();
                tokio::spawn(async move {
                    run_owned_load(context, catalog, state, reservation, entry, response).await;
                });
                receiver.await.unwrap_or_else(|_| {
                    CommandResult::immediate(LoaderResponse::error(
                        "runtime-owned Loader task failed",
                    ))
                })
            }
            LoaderCommand::Reconfigure { id, config } => {
                let handle = self
                    .state
                    .lock()
                    .expect("loader state poisoned")
                    .handles
                    .get(&id)
                    .cloned();
                match handle {
                    Some(handle) => {
                        CommandResult::immediate(match handle.reconfigure(config).await {
                            Ok(snapshot) => LoaderResponse::fiber(snapshot),
                            Err(error) => LoaderResponse::error(error),
                        })
                    }
                    None => {
                        CommandResult::immediate(LoaderResponse::error("unknown loader entry id"))
                    }
                }
            }
            LoaderCommand::Unload { id } => {
                let handle = {
                    let mut state = self.state.lock().expect("loader state poisoned");
                    state.handles.remove(&id)
                };
                match handle {
                    Some(handle) => {
                        let reservation = IdReservation::retain_until_released(&self.state, id);
                        let response = match tokio::spawn(async move {
                            let _reservation = reservation;
                            let report = handle.dispose().await;
                            (handle, report)
                        })
                        .await
                        {
                            Ok((handle, report)) if report.is_clean() => {
                                LoaderResponse::fiber(handle.snapshot())
                            }
                            Ok((_, report)) => {
                                LoaderResponse::error(format!("cleanup failed: {report:?}"))
                            }
                            Err(error) => LoaderResponse::error(error),
                        };
                        CommandResult::immediate(response)
                    }
                    None => {
                        CommandResult::immediate(LoaderResponse::error("unknown loader entry id"))
                    }
                }
            }
            LoaderCommand::Inspect => {
                let fibers = self
                    .state
                    .lock()
                    .expect("loader state poisoned")
                    .handles
                    .iter()
                    .map(|(id, handle)| (id.clone(), handle.snapshot()))
                    .collect();
                CommandResult::immediate(LoaderResponse {
                    ok: true,
                    fiber: None,
                    fibers: Some(fibers),
                    error: None,
                })
            }
        }
    }
}

async fn run_owned_load(
    context: Context,
    catalog: NativeCatalog,
    state: Arc<Mutex<LoaderState>>,
    reservation: IdReservation,
    entry: LoaderEntry,
    response: oneshot::Sender<CommandResult>,
) {
    let artifact = entry.artifact;
    let factory = match tokio::task::spawn_blocking(move || catalog.load(artifact)).await {
        Ok(Ok(factory)) => factory,
        Ok(Err(error)) => {
            let _ = response.send(CommandResult::immediate(LoaderResponse::error(error)));
            return;
        }
        Err(error) => {
            let _ = response.send(CommandResult::immediate(LoaderResponse::error(error)));
            return;
        }
    };
    if response.is_closed() {
        return;
    }
    let handle = match context.apply(factory, entry.config).await {
        Ok(handle) => handle,
        Err(error) => {
            let _ = response.send(CommandResult::immediate(LoaderResponse::error(error)));
            return;
        }
    };
    let snapshot = handle.snapshot();
    let rollback_handle = handle.clone();
    let id = reservation.publish(handle);
    let (delivered, delivery) = oneshot::channel();
    if response
        .send(CommandResult {
            response: LoaderResponse::fiber(snapshot),
            delivered: Some(delivered),
        })
        .is_ok()
        && delivery.await.is_ok()
    {
        return;
    }

    let removed = state
        .lock()
        .expect("loader state poisoned")
        .handles
        .remove(&id);
    if removed.is_some() {
        let reservation = IdReservation::retain_until_released(&state, id);
        let _reservation = reservation;
        let _ = rollback_handle.dispose().await;
    }
}

impl LoaderResponse {
    fn fiber(fiber: FiberSnapshot) -> Self {
        Self {
            ok: true,
            fiber: Some(fiber),
            fibers: None,
            error: None,
        }
    }

    #[allow(clippy::needless_pass_by_value)] // Accepts owned errors and static messages uniformly.
    fn error(error: impl ToString) -> Self {
        Self {
            ok: false,
            fiber: None,
            fibers: None,
            error: Some(error.to_string()),
        }
    }
}
