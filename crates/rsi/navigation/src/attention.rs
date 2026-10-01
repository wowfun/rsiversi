use super::{
    Arc, BTreeMap, BoxFuture, Config, ConfigValue, Deserialize, Domain, DomainFacilityContract,
    DomainSpec, Execution, Mutex, PluginFactory, PreparedActivation, Result, Semaphore,
    TaskTracker, activation, session_error, storage_error,
};
use async_trait::async_trait;
use rsi_acp_protocol::{
    observation::Status as ExternalStatus,
    service::{ExternalConversations, ExternalConversationsContract},
};
use rsi_api_protocol::{
    ApiError, ApiRegistrarContract, CallOrigin, ConnectionDescriptionContract, HostEpoch,
    json_handler,
};
use rsi_conversation::ConversationIdentity;
use rsi_meta::ActivationPlan;
use rsi_navigation_api::{
    attention::{Entry, MarkRead, Operation, Page, Position, Status, Target},
    revision,
};
use rsi_session_protocol::{ActivityStatus, SessionIngress, SessionIngressContract};
use rsi_storage_domain::{RecordObjectSize, encoded_entry_bytes};
use sha2::Digest as _;

struct ReadingPosition {
    position: Position,
    encoded_bytes: usize,
}
struct State {
    closed: bool,
    positions: BTreeMap<String, ReadingPosition>,
    recency: std::collections::VecDeque<String>,
    size: RecordObjectSize,
}
impl State {
    // Snapshot bounds, keys and Position values are validated before construction.
    // Cached sizes stay paired with their values; a one-entry projection cannot
    // overflow usize, and removal always names an existing entry.
    fn from_positions(positions: BTreeMap<String, Position>) -> Self {
        let mut size = RecordObjectSize::default();
        let recency = positions.keys().cloned().collect();
        let positions = positions
            .into_iter()
            .map(|(key, position)| {
                let encoded_bytes = position_bytes(&key, &position);
                size = size
                    .with_entry(None, encoded_bytes)
                    .expect("bounded positions");
                (
                    key,
                    ReadingPosition {
                        position,
                        encoded_bytes,
                    },
                )
            })
            .collect();
        Self {
            closed: false,
            positions,
            recency,
            size,
        }
    }

    fn remove(&mut self, key: &str) {
        let previous = self
            .positions
            .remove(key)
            .expect("retained reading position");
        self.size = self
            .size
            .without_entry(previous.encoded_bytes)
            .expect("bounded positions");
        self.recency.retain(|entry| entry != key);
    }

    fn put(&mut self, key: String, position: Position, encoded_bytes: usize) {
        let previous = self.positions.insert(
            key.clone(),
            ReadingPosition {
                position,
                encoded_bytes,
            },
        );
        self.size = self
            .size
            .with_entry(previous.map(|record| record.encoded_bytes), encoded_bytes)
            .expect("bounded positions");
        self.recency.retain(|entry| entry != &key);
        self.recency.push_back(key);
    }
}
fn position_bytes(key: &str, position: &Position) -> usize {
    encoded_entry_bytes(
        key,
        serde_json::to_vec(position)
            .expect("typed reading position")
            .len(),
    )
    .expect("bounded reading position")
}
struct Attention {
    session: Arc<dyn SessionIngress>,
    external: Arc<dyn ExternalConversations>,
    epoch: HostEpoch,
    domain: Arc<dyn Domain>,
    state: Mutex<State>,
    execution: Execution,
    tasks: TaskTracker,
    slots: Arc<Semaphore>,
    writer: Arc<Semaphore>,
}
fn key(origin: &CallOrigin, id: &ConversationIdentity) -> String {
    let principal = match origin {
        CallOrigin::Local => "local".to_owned(),
        CallOrigin::Device(device) => format!("device:{}", device.id.as_str()),
    };
    hex::encode(sha2::Sha256::digest(
        serde_json::to_vec(&(principal, id)).expect("typed identity"),
    ))
}
impl Attention {
    fn run<T: Send + 'static>(
        self: &Arc<Self>,
        work: impl FnOnce(Arc<Self>) -> BoxFuture<'static, Result<T>>,
    ) -> Result<BoxFuture<'static, Result<T>>> {
        self.domain.ensure_available().map_err(storage_error)?;
        let state = self.state.lock().expect("attention admission");
        if state.closed {
            return Err(ApiError::ShuttingDown);
        }
        let permit = self
            .slots
            .clone()
            .try_acquire_owned()
            .map_err(|_| ApiError::Capacity)?;
        let future = work(self.clone());
        let task = self.execution.spawn(self.tasks.track_future(async move {
            let _permit = permit;
            future.await
        }));
        drop(state);
        Ok(Box::pin(async move {
            task.await.map_err(|_| ApiError::OutcomeUnknown)?
        }))
    }
    async fn candidates(&self, origin: &CallOrigin) -> Result<Page> {
        self.domain.ensure_available().map_err(storage_error)?;
        let native = self
            .session
            .scoped(origin.clone())
            .activity()
            .await
            .map_err(session_error)?;
        let external = self
            .external
            .residents()
            .await
            .map_err(|_| ApiError::Unavailable)?;
        let mut entries: Vec<_> = native
            .entries
            .into_iter()
            .map(|row| Entry {
                position: Position {
                    conversation: ConversationIdentity::Native(row.session),
                    epoch: "0".into(),
                    sequence: row.fact_seq,
                },
                status: if row.requests.is_empty() {
                    match row.status {
                        ActivityStatus::Running => Status::Running,
                        ActivityStatus::Idle => Status::Unread,
                        ActivityStatus::Unknown => Status::Unknown,
                    }
                } else {
                    Status::Waiting
                },
                targets: row
                    .requests
                    .into_iter()
                    .map(|request| Target::Native { request })
                    .collect(),
            })
            .collect();
        for row in external {
            let status = if (!row.connected && row.snapshot.status != ExternalStatus::Closed)
                || row.snapshot.status == ExternalStatus::Unknown
            {
                Status::Unknown
            } else if !row.permissions.is_empty() {
                Status::Waiting
            } else if matches!(
                row.snapshot.status,
                ExternalStatus::Starting | ExternalStatus::Loading | ExternalStatus::Running
            ) {
                Status::Running
            } else {
                Status::Unread
            };
            let targets = if status == Status::Waiting {
                row.permissions
                    .into_iter()
                    .map(|permission| Target::External {
                        generation: permission.generation,
                        request: permission.id,
                    })
                    .collect()
            } else {
                Vec::new()
            };
            entries.push(Entry {
                position: Position {
                    conversation: ConversationIdentity::External(row.snapshot.id),
                    epoch: row.snapshot.epoch.to_string(),
                    sequence: row.sequence,
                },
                status,
                targets,
            });
        }
        Ok(Page {
            host_epoch: self.epoch.clone(),
            entries,
            truncated: native.truncated,
        })
    }
    async fn read(&self, origin: &CallOrigin) -> Result<Page> {
        let mut page = self.candidates(origin).await?;
        page.entries.sort_by(|a, b| {
            (&a.status, &a.position.conversation).cmp(&(&b.status, &b.position.conversation))
        });
        let keys: Vec<_> = page
            .entries
            .iter()
            .map(|row| key(origin, &row.position.conversation))
            .collect();
        let mut keys = keys.iter();
        let state = self.state.lock().expect("attention reading positions");
        self.domain.ensure_available().map_err(storage_error)?;
        page.entries.retain(|row| {
            let key = keys.next().expect("one key per candidate");
            row.status != Status::Unread
                || (row.position.sequence != "0"
                    && state.positions.get(key).is_none_or(|read| {
                        read.position.epoch != row.position.epoch
                            || revision(&read.position.sequence).unwrap_or(0)
                                < revision(&row.position.sequence).unwrap_or(0)
                    }))
        });
        drop(state);
        let mut bytes = 1024;
        page.entries.retain(|row| {
            bytes += serde_json::to_vec(row).expect("typed attention row").len() + 1;
            if bytes <= 256 * 1024 {
                true
            } else {
                page.truncated = true;
                false
            }
        });
        page.validate()?;
        Ok(page)
    }
    async fn mark(&self, origin: CallOrigin, request: MarkRead) -> Result<Position> {
        let _writer = self.writer.try_acquire().map_err(|_| ApiError::Capacity)?;
        request.position.validate()?;
        if request.host_epoch != self.epoch {
            return Err(ApiError::Invalid("attention Host changed".into()));
        }
        let page = self.candidates(&origin).await?;
        self.domain.ensure_available().map_err(storage_error)?;
        let current = page
            .entries
            .iter()
            .find(|row| row.position.conversation == request.position.conversation)
            .ok_or_else(|| ApiError::Invalid("attention source is no longer managed".into()))?;
        if current.position.epoch != request.position.epoch
            || revision(&request.position.sequence)? > revision(&current.position.sequence)?
        {
            return Err(ApiError::Invalid(
                "attention source changed or position is in the future".into(),
            ));
        }
        let key = key(&origin, &request.position.conversation);
        let mut position = request.position;
        {
            let state = self.state.lock().expect("attention reading positions");
            self.domain.ensure_available().map_err(storage_error)?;
            if let Some(previous) = state.positions.get(&key)
                && previous.position.epoch == position.epoch
                && revision(&previous.position.sequence)? > revision(&position.sequence)?
            {
                position.sequence.clone_from(&previous.position.sequence);
            }
        }
        // The writer permit keeps these cached sizes stable through durable publication.
        let encoded_bytes = position_bytes(&key, &position);
        let evicted = {
            let state = self.state.lock().expect("attention reading positions");
            let mut projected = state
                .size
                .with_entry(
                    state.positions.get(&key).map(|record| record.encoded_bytes),
                    encoded_bytes,
                )
                .expect("bounded positions");
            let mut evicted = Vec::new();
            let mut candidates = state.recency.iter().filter(|candidate| *candidate != &key);
            let spec = self.domain.spec();
            while projected.records() > spec.maximum_records
                || projected.bytes() > spec.maximum_bytes
            {
                let oldest = candidates.next().ok_or(ApiError::Capacity)?;
                projected = projected
                    .without_entry(state.positions[oldest].encoded_bytes)
                    .expect("bounded positions");
                evicted.push(oldest.clone());
            }
            evicted
        };
        for oldest in &evicted {
            self.domain.delete(oldest).await.map_err(storage_error)?;
            // Each acknowledged eviction is already durable even if the next write fails.
            let mut state = self.state.lock().expect("attention reading positions");
            state.remove(oldest);
        }
        self.domain
            .put(
                &key,
                serde_json::to_value(&position).expect("typed reading position"),
            )
            .await
            .map_err(storage_error)?;
        let mut state = self.state.lock().expect("attention reading positions");
        state.put(key, position.clone(), encoded_bytes);
        Ok(position)
    }
    async fn close(&self) {
        {
            let mut state = self.state.lock().expect("attention admission");
            state.closed = true;
            self.slots.close();
            self.writer.close();
            self.tasks.close();
        }
        self.tasks.wait().await;
    }
}
/// Ordinary Host owner of bounded native/external attention and reading positions.
#[derive(Clone, Debug, Default)]
pub struct AttentionFactory;
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Empty {}
#[derive(serde::Serialize)]
enum Never {}
#[async_trait]
impl PluginFactory for AttentionFactory {
    fn prepare(&self, config: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        let input: Config = serde_json::from_value(config.clone()).map_err(|_| {
            rsi_meta::MetaError::InvalidInput("invalid attention configuration".into())
        })?;
        if input.backend.is_empty() || input.backend.len() > 256 {
            return Err(rsi_meta::MetaError::InvalidInput(
                "invalid attention backend".into(),
            ));
        }
        Ok(PreparedActivation::new(config.clone())
            .requiring_local::<DomainFacilityContract>()
            .requiring_local::<SessionIngressContract>()
            .requiring_local::<ExternalConversationsContract>()
            .requiring_local::<ConnectionDescriptionContract>()
            .requiring_local::<ApiRegistrarContract>())
    }
    async fn activate(&self, plan: ActivationPlan) -> rsi_meta::Result<()> {
        let input: Config =
            serde_json::from_value(plan.config().as_ref().clone()).map_err(activation)?;
        let domain = plan
            .local::<DomainFacilityContract>()?
            .open(DomainSpec {
                id: "rsi.attention".into(),
                backend: input.backend,
                version: 1,
                maximum_records: 4096,
                maximum_bytes: 1024 * 1024,
            })
            .await
            .map_err(activation)?;
        let mut positions = BTreeMap::new();
        for (key, value) in domain.snapshot().await.map_err(activation)? {
            if key.len() != 64
                || !key
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            {
                return Err(activation("invalid attention reading key"));
            }
            let position: Position = serde_json::from_value(value).map_err(activation)?;
            position.validate().map_err(activation)?;
            positions.insert(key, position);
        }
        let owner = Arc::new(Attention {
            session: plan.local::<SessionIngressContract>()?,
            external: plan.local::<ExternalConversationsContract>()?,
            epoch: plan
                .local::<ConnectionDescriptionContract>()?
                .host_epoch
                .clone(),
            domain,
            state: Mutex::new(State::from_positions(positions)),
            execution: plan.context().runtime().execution().clone(),
            tasks: TaskTracker::new(),
            slots: Arc::new(Semaphore::new(2)),
            writer: Arc::new(Semaphore::new(1)),
        });
        let registrar = plan.local::<ApiRegistrarContract>()?;
        let read = owner.clone();
        let read = registrar
            .register(
                Operation::Read.spec(),
                json_handler(move |context, _: Empty| {
                    let read = read.clone();
                    async move {
                        read.run(move |owner| {
                            Box::pin(async move { owner.read(&context.origin).await })
                        })?
                        .await
                        .map(Ok::<_, Never>)
                    }
                }),
            )
            .map_err(activation)?;
        let write = owner.clone();
        let write = registrar
            .register(
                Operation::MarkRead.spec(),
                json_handler(move |context, request: MarkRead| {
                    let write = write.clone();
                    async move {
                        write
                            .run(move |owner| {
                                Box::pin(async move { owner.mark(context.origin, request).await })
                            })?
                            .await
                            .map(Ok::<_, Never>)
                    }
                }),
            )
            .map_err(activation)?;
        plan.defer(
            "drain attention navigation",
            Box::new(move || {
                Box::pin(async move {
                    futures_util::join!(owner.close(), read.close(), write.close());
                    Ok(())
                })
            }),
        )
    }
}

#[cfg(test)]
#[path = "attention_tests.rs"]
mod tests;
