use super::catalog::LoadAdmission;
use super::{LOADER_CONTRACT_ID, LOADER_CONTRACT_VERSION, LOADER_SERVICE_KEY, NativeCatalog};
use async_trait::async_trait;
use futures_util::{StreamExt as _, TryStreamExt as _, stream};
use rsi_meta::{
    ActivationPlan, ConfigValue, Context, FactoryIdentity, FiberHandle, FiberSnapshot, Message,
    MetaError, PluginFactory, PreparedActivation, ProviderChannel, Result, ServiceEndpoint,
};
use serde::ser::{SerializeMap as _, SerializeStruct as _};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::io;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use tokio::sync::oneshot;

const MAX_CONCURRENT_PREFLIGHTS: usize = 8;
const MAX_LOADER_ENTRIES: usize = 1024;
const MAX_LOADER_ID_BYTES: usize = 128;
const RESPONSE_TOO_LARGE_JSON: &[u8] =
    br#"{"ok":false,"error":"loader response exceeds frame limit"}"#;

fn valid_loader_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= MAX_LOADER_ID_BYTES
}

fn prepare_loader_config(desired: &ConfigValue) -> Result<PreparedActivation> {
    let parsed = LoaderConfig::deserialize(desired)
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
    let normalized =
        serde_json::to_value(parsed).map_err(|error| MetaError::InvalidInput(error.to_string()))?;
    Ok(PreparedActivation::new(normalized))
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
}

impl LoaderFactory {
    pub fn new(catalog: NativeCatalog) -> Self {
        Self { catalog }
    }
}

#[async_trait]
impl PluginFactory for LoaderFactory {
    fn identity(&self) -> FactoryIdentity {
        FactoryIdentity::builtin("rsi.meta.loader", "1")
    }

    fn prepare(&self, desired: &ConfigValue) -> Result<PreparedActivation> {
        prepare_loader_config(desired)
    }

    async fn activate(&self, plan: ActivationPlan) -> Result<()> {
        let parsed = LoaderConfig::deserialize(plan.config().as_ref())
            .map_err(|error| MetaError::InvalidInput(error.to_string()))?;
        let context = plan.context().clone();
        let preflight_width = self
            .catalog
            .load_concurrency_limit()
            .min(MAX_CONCURRENT_PREFLIGHTS);
        let catalog = self.catalog.clone();
        let loaded = stream::iter(parsed.entries.into_iter().enumerate())
            .map(move |(position, entry)| {
                let catalog = catalog.clone();
                async move {
                    let LoaderEntry {
                        id,
                        artifact,
                        config,
                    } = entry;
                    let load_admission = catalog
                        .try_reserve_load()
                        .map_err(|error| MetaError::Activation(error.to_string()))?;
                    let worker_admission = load_admission.clone();
                    let factory = tokio::task::spawn_blocking(move || {
                        catalog.load_admitted(artifact, &worker_admission)
                    })
                    .await
                    .map_err(|error| MetaError::Activation(error.to_string()))?
                    .map_err(|error| MetaError::Activation(error.to_string()))?;
                    drop(load_admission);
                    Ok::<_, MetaError>((position, id, config, factory))
                }
            })
            .buffered(preflight_width)
            .try_collect::<Vec<_>>()
            .await?;
        let mut module_groups = BTreeMap::<_, Vec<_>>::new();
        for (position, id, config, factory) in loaded {
            module_groups
                .entry(factory.module_digest().to_owned())
                .or_default()
                .push((position, id, config, factory));
        }
        let runtime = context.runtime().clone();
        let preparation_width =
            preflight_width.min(runtime.limits().execution.maximum_concurrent_preparations);
        let prepared_groups = stream::iter(module_groups.into_values())
            .map(move |group| {
                let runtime = runtime.clone();
                async move {
                    let mut prepared = Vec::with_capacity(group.len());
                    for (position, id, config, factory) in group {
                        let plugin = tokio::task::spawn_blocking({
                            let runtime = runtime.clone();
                            move || runtime.prepare(factory, config)
                        })
                        .await
                        .map_err(|error| MetaError::Activation(error.to_string()))??;
                        prepared.push((position, id, plugin));
                    }
                    Ok::<_, MetaError>(prepared)
                }
            })
            .buffer_unordered(preparation_width)
            .try_collect::<Vec<_>>()
            .await?;
        let prepared = prepared_groups
            .into_iter()
            .flatten()
            .map(|(position, id, plugin)| (position, (id, plugin)))
            .collect::<BTreeMap<_, _>>();

        let state = Arc::new(Mutex::new(LoaderState::default()));
        for (_, (id, plugin)) in prepared {
            let handle = context.apply_prepared(plugin).await?;
            IdReservation::claim(&state, id)
                .map_err(|error| MetaError::Activation(error.to_owned()))?
                .publish(handle);
        }
        // The activation root effect retains this supply through generation
        // retirement; the exact SupplyHandle is needed only for early withdrawal.
        let _loader_service = context.provide(
            LOADER_SERVICE_KEY,
            LOADER_CONTRACT_ID,
            LOADER_CONTRACT_VERSION,
            Arc::new(LoaderEndpoint {
                context: context.clone(),
                catalog: self.catalog.clone(),
                state,
            }),
        )?;
        Ok(())
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
    error: Option<String>,
}

enum CommandResponse {
    Immediate(LoaderResponse),
    Inspect,
}

struct CommandResult {
    response: CommandResponse,
    delivered: Option<oneshot::Sender<()>>,
}

impl CommandResult {
    fn immediate(response: LoaderResponse) -> Self {
        Self {
            response: CommandResponse::Immediate(response),
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
    slots: BTreeMap<String, LoaderSlot>,
}

struct LoaderSlot {
    token: Arc<IdToken>,
    handle: Option<FiberHandle>,
}

#[derive(Debug)]
struct IdToken;

#[derive(Clone)]
struct IdClaim {
    id: String,
    token: Arc<IdToken>,
}

struct PublishedId {
    claim: IdClaim,
}

struct IdReservation {
    state: Arc<Mutex<LoaderState>>,
    claim: Option<IdClaim>,
}

impl IdReservation {
    fn claim(
        state: &Arc<Mutex<LoaderState>>,
        id: String,
    ) -> std::result::Result<Self, &'static str> {
        let mut loader = state.lock().expect("loader state poisoned");
        if loader.slots.contains_key(&id) {
            return Err("loader entry id already exists");
        }
        if loader.slots.len() >= MAX_LOADER_ENTRIES {
            return Err("loader entry capacity is exhausted");
        }
        let claim = IdClaim {
            id,
            token: Arc::new(IdToken),
        };
        loader.slots.insert(
            claim.id.clone(),
            LoaderSlot {
                token: Arc::clone(&claim.token),
                handle: None,
            },
        );
        drop(loader);
        Ok(Self {
            state: Arc::clone(state),
            claim: Some(claim),
        })
    }

    fn publish(mut self, handle: FiberHandle) -> PublishedId {
        let claim = self.claim.take().expect("unpublished reservation");
        let mut loader = self.state.lock().expect("loader state poisoned");
        let slot = loader
            .slots
            .get_mut(&claim.id)
            .expect("a reservation retains its Loader slot");
        assert!(Arc::ptr_eq(&slot.token, &claim.token));
        assert!(slot.handle.replace(handle).is_none());
        drop(loader);
        PublishedId { claim }
    }

    fn retire(state: &Arc<Mutex<LoaderState>>, id: &str) -> Option<(FiberHandle, Self)> {
        let mut loader = state.lock().expect("loader state poisoned");
        let slot = loader.slots.get_mut(id)?;
        let handle = slot.handle.take()?;
        let claim = IdClaim {
            id: id.to_owned(),
            token: Arc::clone(&slot.token),
        };
        Some((
            handle,
            Self {
                state: Arc::clone(state),
                claim: Some(claim),
            },
        ))
    }

    fn retire_published(state: &Arc<Mutex<LoaderState>>, published: &PublishedId) -> Option<Self> {
        let mut loader = state.lock().expect("loader state poisoned");
        let slot = loader.slots.get_mut(&published.claim.id)?;
        if !Arc::ptr_eq(&slot.token, &published.claim.token) {
            return None;
        }
        // Only the caller that transitions this generation out of the
        // published state owns its retirement reservation. A concurrent unload
        // may already be retaining the same token through cleanup.
        slot.handle.take()?;
        Some(Self {
            state: Arc::clone(state),
            claim: Some(published.claim.clone()),
        })
    }
}

impl Drop for IdReservation {
    fn drop(&mut self) {
        if let Some(claim) = self.claim.take() {
            let mut loader = self.state.lock().expect("loader state poisoned");
            let matches = loader.slots.get(&claim.id).is_some_and(|slot| {
                slot.handle.is_none() && Arc::ptr_eq(&slot.token, &claim.token)
            });
            if matches {
                loader.slots.remove(&claim.id);
            }
        }
    }
}

impl LoaderState {
    fn handle(&self, id: &str) -> Option<FiberHandle> {
        self.slots.get(id).and_then(|slot| slot.handle.clone())
    }

    fn handles(&self) -> BTreeMap<String, FiberHandle> {
        self.slots
            .iter()
            .filter_map(|(id, slot)| slot.handle.clone().map(|handle| (id.clone(), handle)))
            .collect()
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
        invocation: rsi_meta::InvocationContext,
        mut channel: ProviderChannel<'_>,
    ) -> Result<()> {
        let maximum_message_bytes = invocation
            .provider_context()
            .runtime()
            .limits()
            .payloads
            .maximum_message_bytes;
        let cancellation = channel.cancellation();
        while let Some(message) = channel.recv().await {
            let mut result = match serde_json::from_slice::<LoaderCommand>(message.as_bytes()) {
                Ok(command) => {
                    tokio::select! {
                        biased;
                        () = cancellation.cancelled() => return Ok(()),
                        response = self.execute(command) => response,
                    }
                }
                Err(error) => CommandResult::immediate(LoaderResponse::error(error)),
            };
            let response =
                serialize_response(&result.response, &self.state, maximum_message_bytes)?;
            channel.send(response.message).await?;
            if response.complete
                && let Some(delivered) = result.delivered.take()
            {
                let _ = delivered.send(());
            }
        }
        Ok(())
    }
}

struct MessageBudgetWriter {
    bytes: Vec<u8>,
    maximum: usize,
    exceeded: bool,
}

impl MessageBudgetWriter {
    fn new(maximum: usize) -> Self {
        Self {
            bytes: Vec::new(),
            maximum,
            exceeded: false,
        }
    }
}

impl io::Write for MessageBudgetWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let Some(end) = self.bytes.len().checked_add(bytes.len()) else {
            self.exceeded = true;
            return Err(io::Error::other("loader response exceeds frame limit"));
        };
        if end > self.maximum {
            self.exceeded = true;
            return Err(io::Error::other("loader response exceeds frame limit"));
        }
        if end > self.bytes.capacity() {
            let doubled = self.bytes.capacity().max(256).saturating_mul(2);
            let target = end.max(doubled.min(self.maximum));
            self.bytes
                .try_reserve_exact(target.saturating_sub(self.bytes.len()))
                .map_err(io::Error::other)?;
        }
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

struct SerializedResponse {
    message: Message,
    complete: bool,
}

fn serialize_response(
    response: &CommandResponse,
    state: &Arc<Mutex<LoaderState>>,
    maximum: usize,
) -> Result<SerializedResponse> {
    match response {
        CommandResponse::Immediate(response) => serialize_value(response, maximum),
        CommandResponse::Inspect => {
            // FiberHandle clones retain only Runtime-owned handles. Snapshot
            // construction and bounded JSON serialization must not exclude
            // concurrent load, unload, or reconfigure registry operations.
            let handles = state.lock().expect("loader state poisoned").handles();
            serialize_value(
                &InspectResponse {
                    entries: &handles,
                    snapshot: FiberHandle::snapshot,
                },
                maximum,
            )
        }
    }
}

fn serialize_value(response: &impl Serialize, maximum: usize) -> Result<SerializedResponse> {
    let mut writer = MessageBudgetWriter::new(maximum);
    match serde_json::to_writer(&mut writer, response) {
        Ok(()) => Ok(SerializedResponse {
            message: Message::new(writer.bytes),
            complete: true,
        }),
        Err(_) if writer.exceeded => {
            drop(writer);
            if RESPONSE_TOO_LARGE_JSON.len() > maximum {
                Err(MetaError::PayloadTooLarge { maximum })
            } else {
                Ok(SerializedResponse {
                    message: Message::new(RESPONSE_TOO_LARGE_JSON.to_vec()),
                    complete: false,
                })
            }
        }
        Err(error) => Err(MetaError::Service(format!(
            "loader response serialization failed: {error}"
        ))),
    }
}

struct InspectResponse<'a, Value, Snapshot> {
    entries: &'a BTreeMap<String, Value>,
    snapshot: Snapshot,
}

impl<Value, Snapshot> Serialize for InspectResponse<'_, Value, Snapshot>
where
    Snapshot: Fn(&Value) -> FiberSnapshot,
{
    fn serialize<Serializer>(
        &self,
        serializer: Serializer,
    ) -> std::result::Result<Serializer::Ok, Serializer::Error>
    where
        Serializer: serde::Serializer,
    {
        let mut response = serializer.serialize_struct("LoaderResponse", 2)?;
        response.serialize_field("ok", &true)?;
        response.serialize_field(
            "fibers",
            &LazySnapshots {
                entries: self.entries,
                snapshot: &self.snapshot,
            },
        )?;
        response.end()
    }
}

struct LazySnapshots<'a, Value, Snapshot> {
    entries: &'a BTreeMap<String, Value>,
    snapshot: &'a Snapshot,
}

impl<Value, Snapshot> Serialize for LazySnapshots<'_, Value, Snapshot>
where
    Snapshot: Fn(&Value) -> FiberSnapshot,
{
    fn serialize<Serializer>(
        &self,
        serializer: Serializer,
    ) -> std::result::Result<Serializer::Ok, Serializer::Error>
    where
        Serializer: serde::Serializer,
    {
        let mut snapshots = serializer.serialize_map(Some(self.entries.len()))?;
        for (id, handle) in self.entries {
            snapshots.serialize_entry(id, &(self.snapshot)(handle))?;
        }
        snapshots.end()
    }
}

impl LoaderEndpoint {
    async fn execute(&self, command: LoaderCommand) -> CommandResult {
        match command {
            LoaderCommand::Load(entry) => self.execute_load(entry).await,
            LoaderCommand::Reconfigure { id, config } => {
                let handle = self
                    .state
                    .lock()
                    .expect("loader state poisoned")
                    .handle(&id);
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
                match IdReservation::retire(&self.state, &id) {
                    Some((handle, reservation)) => {
                        // The service waiter may be cancelled while disposal is
                        // still running. A Runtime-owned disposal persists, but
                        // this task must also retain the Loader ID until that
                        // disposal finishes so a replacement cannot overlap it.
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
            LoaderCommand::Inspect => CommandResult {
                response: CommandResponse::Inspect,
                delivered: None,
            },
        }
    }

    async fn execute_load(&self, entry: LoaderEntry) -> CommandResult {
        if !valid_loader_id(&entry.id) {
            return CommandResult::immediate(LoaderResponse::error("invalid loader entry id"));
        }
        let load_admission = match self.catalog.try_reserve_load() {
            Ok(admission) => admission,
            Err(error) => return CommandResult::immediate(LoaderResponse::error(error)),
        };
        let reservation = match IdReservation::claim(&self.state, entry.id.clone()) {
            Ok(reservation) => reservation,
            Err(error) => return CommandResult::immediate(LoaderResponse::error(error)),
        };
        let context = self.context.clone();
        let catalog = self.catalog.clone();
        let state = Arc::clone(&self.state);
        let (response, receiver) = oneshot::channel();
        tokio::spawn(async move {
            run_owned_load(
                context,
                catalog,
                state,
                reservation,
                load_admission,
                entry,
                response,
            )
            .await;
        });
        receiver.await.unwrap_or_else(|_| {
            CommandResult::immediate(LoaderResponse::error("runtime-owned Loader task failed"))
        })
    }
}

async fn run_owned_load(
    context: Context,
    catalog: NativeCatalog,
    state: Arc<Mutex<LoaderState>>,
    reservation: IdReservation,
    load_admission: LoadAdmission,
    entry: LoaderEntry,
    response: oneshot::Sender<CommandResult>,
) {
    let artifact = entry.artifact;
    let worker_admission = load_admission.clone();
    // This owner outlives native loading and remains through delivery or
    // rollback; the blocking worker holds only a clone for its call boundary.
    let _owned_load_admission = load_admission;
    let factory = match tokio::task::spawn_blocking(move || {
        catalog.load_admitted(artifact, &worker_admission)
    })
    .await
    {
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
    let published = reservation.publish(handle);
    let (delivered, delivery) = oneshot::channel();
    if response
        .send(CommandResult {
            response: CommandResponse::Immediate(LoaderResponse::fiber(snapshot)),
            delivered: Some(delivered),
        })
        .is_ok()
        && delivery.await.is_ok()
    {
        return;
    }

    let _reservation = IdReservation::retire_published(&state, &published);
    let _ = rollback_handle.dispose().await;
}

impl LoaderResponse {
    fn fiber(fiber: FiberSnapshot) -> Self {
        Self {
            ok: true,
            fiber: Some(fiber),
            error: None,
        }
    }

    #[allow(clippy::needless_pass_by_value)] // Accepts owned errors and static messages uniformly.
    fn error(error: impl ToString) -> Self {
        Self {
            ok: false,
            fiber: None,
            error: Some(error.to_string()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rsi_meta::{FiberGeneration, FiberId, FiberState, Requirement};
    use serde_json::json;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[derive(Debug)]
    struct PassiveFactory {
        identity: FactoryIdentity,
    }

    #[async_trait]
    impl PluginFactory for PassiveFactory {
        fn identity(&self) -> FactoryIdentity {
            self.identity.clone()
        }

        fn prepare(&self, desired: &ConfigValue) -> Result<PreparedActivation> {
            Ok(PreparedActivation::new(desired.clone()))
        }

        async fn activate(&self, _plan: ActivationPlan) -> Result<()> {
            Ok(())
        }
    }

    fn passive_factory(name: &'static str) -> Arc<dyn PluginFactory> {
        Arc::new(PassiveFactory {
            identity: FactoryIdentity::builtin(name, "1"),
        })
    }

    #[derive(Debug)]
    struct LoaderProbeFactory {
        response: Arc<Mutex<Option<Value>>>,
    }

    #[async_trait]
    impl PluginFactory for LoaderProbeFactory {
        fn identity(&self) -> FactoryIdentity {
            FactoryIdentity::builtin("loader-probe", "1")
        }

        fn prepare(&self, desired: &ConfigValue) -> Result<PreparedActivation> {
            Ok(
                PreparedActivation::new(desired.clone()).requiring(Requirement::new(
                    LOADER_SERVICE_KEY,
                    LOADER_CONTRACT_ID,
                    LOADER_CONTRACT_VERSION,
                )),
            )
        }

        async fn activate(&self, plan: ActivationPlan) -> Result<()> {
            let service = plan.inject(LOADER_SERVICE_KEY).ok_or_else(|| {
                MetaError::Activation("Loader service was not injected".to_owned())
            })?;
            let response = service
                .invoke(Message::new(br#"{"operation":"inspect"}"#.as_slice()))
                .await?;
            let response = serde_json::from_slice(response.as_bytes())
                .map_err(|error| MetaError::Activation(error.to_string()))?;
            *self.response.lock().expect("probe response poisoned") = Some(response);
            Ok(())
        }
    }

    #[test]
    fn preparation_retains_the_exact_normalized_loader_config() {
        let desired = json!({
            "entries": [{
                "id": "normalized",
                "artifact": "plugin.so"
            }]
        });

        let prepared = prepare_loader_config(&desired).unwrap();

        assert_eq!(
            prepared.config(),
            &json!({
                "entries": [{
                    "id": "normalized",
                    "artifact": "plugin.so",
                    "config": null
                }]
            })
        );
        assert!(prepared.requirements().is_empty());
        assert_eq!(
            desired,
            json!({
                "entries": [{
                    "id": "normalized",
                    "artifact": "plugin.so"
                }]
            })
        );
    }

    #[tokio::test]
    async fn activation_root_retains_the_loader_supply_after_activation_returns() {
        let cache = tempfile::tempdir().unwrap();
        let catalog = NativeCatalog::new(crate::CatalogOptions::new(cache.path())).unwrap();
        let runtime = rsi_meta::Runtime::default();
        let loader = runtime
            .root()
            .apply(
                Arc::new(LoaderFactory::new(catalog)),
                json!({ "entries": [] }),
            )
            .await
            .unwrap();
        assert!(matches!(
            loader.wait_settled().await.state,
            FiberState::Active
        ));

        let response = Arc::new(Mutex::new(None));
        let probe = runtime
            .root()
            .apply(
                Arc::new(LoaderProbeFactory {
                    response: Arc::clone(&response),
                }),
                Value::Null,
            )
            .await
            .unwrap();
        assert!(matches!(
            probe.wait_settled().await.state,
            FiberState::Active
        ));
        assert_eq!(
            *response.lock().expect("probe response poisoned"),
            Some(json!({ "ok": true, "fibers": {} }))
        );

        assert!(probe.dispose().await.is_clean());
        assert!(loader.dispose().await.is_clean());
        assert!(runtime.shutdown().await.is_complete());
    }

    #[test]
    fn dynamic_id_claims_are_bounded_before_fiber_publication() {
        let state = Arc::new(Mutex::new(LoaderState::default()));
        let reservations = (0..MAX_LOADER_ENTRIES)
            .map(|index| IdReservation::claim(&state, format!("entry-{index}")).unwrap())
            .collect::<Vec<_>>();

        assert!(matches!(
            IdReservation::claim(&state, "overflow".to_owned()),
            Err("loader entry capacity is exhausted")
        ));
        drop(reservations);
        IdReservation::claim(&state, "reused".to_owned())
            .expect("released ID capacity must be reusable");
    }

    #[tokio::test]
    async fn stale_rollback_cannot_remove_a_reused_loader_id() {
        let runtime = rsi_meta::Runtime::default();
        let first = runtime
            .root()
            .apply(passive_factory("first"), Value::Null)
            .await
            .unwrap();
        let second = runtime
            .root()
            .apply(passive_factory("second"), Value::Null)
            .await
            .unwrap();
        let state = Arc::new(Mutex::new(LoaderState::default()));
        let oversized = serialize_value(
            &LoaderResponse::fiber(first.snapshot()),
            RESPONSE_TOO_LARGE_JSON.len(),
        )
        .unwrap();
        assert!(!oversized.complete);

        let first_id = IdReservation::claim(&state, "shared".to_owned())
            .unwrap()
            .publish(first);
        let (unloaded, retirement) = IdReservation::retire(&state, &first_id.claim.id).unwrap();
        assert!(IdReservation::retire_published(&state, &first_id).is_none());
        assert!(
            state
                .lock()
                .expect("loader state poisoned")
                .slots
                .contains_key(&first_id.claim.id)
        );
        drop(unloaded);
        drop(retirement);

        let second_id = IdReservation::claim(&state, first_id.claim.id.clone())
            .unwrap()
            .publish(second.clone());

        assert!(IdReservation::retire_published(&state, &first_id).is_none());

        {
            let loader = state.lock().expect("loader state poisoned");
            assert_eq!(
                loader
                    .handle(&second_id.claim.id)
                    .map(|handle| handle.snapshot()),
                Some(second.snapshot())
            );
            assert!(loader.slots.contains_key(&second_id.claim.id));
        }
        assert!(runtime.shutdown().await.is_clean());
    }

    #[test]
    fn maximum_inspection_uses_a_message_bounded_diagnostic() {
        let entries = (0..MAX_LOADER_ENTRIES)
            .map(|index| (format!("entry-{index}"), index))
            .collect();
        let constructed = AtomicUsize::new(0);
        let response = InspectResponse {
            entries: &entries,
            snapshot: |index: &usize| {
                constructed.fetch_add(1, Ordering::Relaxed);
                FiberSnapshot {
                    id: FiberId(u64::try_from(*index).unwrap()),
                    generation: FiberGeneration(1),
                    factory: FactoryIdentity::builtin(format!("factory-{index}"), "1"),
                    state: FiberState::Failed("x".repeat(64 * 1024)),
                }
            },
        };

        let serialized = serialize_value(&response, RESPONSE_TOO_LARGE_JSON.len()).unwrap();
        assert_eq!(serialized.message.as_bytes(), RESPONSE_TOO_LARGE_JSON);
        assert!(!serialized.complete);
        assert_eq!(constructed.load(Ordering::Relaxed), 1);
        let Err(error) = serialize_value(&response, RESPONSE_TOO_LARGE_JSON.len() - 1) else {
            panic!("a response smaller than the diagnostic must be rejected");
        };
        assert_eq!(
            error,
            MetaError::PayloadTooLarge {
                maximum: RESPONSE_TOO_LARGE_JSON.len() - 1
            }
        );
    }
}
