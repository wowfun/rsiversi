use super::{
    Arc, BTreeMap, BoxFuture, Config, ConfigValue, Deserialize, Domain, DomainFacilityContract,
    DomainSpec, Execution, Mutex, PluginFactory, PreparedActivation, Result, Semaphore,
    TaskTracker, activation, session_error,
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
use rsi_session_protocol::{ActivityStatus, SessionContract, SessionService};
use sha2::Digest as _;

struct State {
    closed: bool,
    uncertain: bool,
    positions: BTreeMap<String, Position>,
    recency: std::collections::VecDeque<String>,
}
struct Attention {
    session: Arc<dyn SessionService>,
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
        let state = self.state.lock().expect("attention admission");
        if state.closed {
            return Err(ApiError::ShuttingDown);
        }
        if state.uncertain {
            return Err(ApiError::OutcomeUnknown);
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
    async fn candidates(&self) -> Result<Page> {
        let native = self.session.activity().await.map_err(session_error)?;
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
        entries.sort_by(|a, b| {
            (&a.status, &a.position.conversation).cmp(&(&b.status, &b.position.conversation))
        });
        Ok(Page {
            host_epoch: self.epoch.clone(),
            entries,
            truncated: native.truncated,
        })
    }
    async fn read(&self, origin: &CallOrigin) -> Result<Page> {
        let mut page = self.candidates().await?;
        let state = self.state.lock().expect("attention reading positions");
        if state.uncertain {
            return Err(ApiError::OutcomeUnknown);
        }
        page.entries.retain(|row| {
            row.status != Status::Unread
                || (row.position.sequence != "0"
                    && state
                        .positions
                        .get(&key(origin, &row.position.conversation))
                        .is_none_or(|read| {
                            read.epoch != row.position.epoch
                                || revision(&read.sequence).unwrap_or(0)
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
        let page = self.candidates().await?;
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
        let evicted = {
            let state = self.state.lock().expect("attention reading positions");
            if let Some(previous) = state.positions.get(&key)
                && previous.epoch == position.epoch
                && revision(&previous.sequence)? > revision(&position.sequence)?
            {
                position.sequence.clone_from(&previous.sequence);
            }
            let value = serde_json::to_value(&position).expect("typed reading position");
            let mut projected = state
                .positions
                .iter()
                .map(|(key, value)| {
                    (
                        key.clone(),
                        serde_json::to_value(value).expect("typed reading position"),
                    )
                })
                .collect::<BTreeMap<_, _>>();
            projected.insert(key.clone(), value);
            let mut evicted = Vec::new();
            let mut candidates = state.recency.iter().filter(|candidate| *candidate != &key);
            while projected.len() > 4096
                || serde_json::to_vec(&projected)
                    .expect("bounded reading positions")
                    .len()
                    > 1024 * 1024
            {
                let oldest = candidates.next().ok_or(ApiError::Capacity)?;
                projected.remove(oldest);
                evicted.push(oldest.clone());
            }
            evicted
        };
        for oldest in &evicted {
            if self.domain.delete(oldest).await.is_err() {
                self.state
                    .lock()
                    .expect("attention reading positions")
                    .uncertain = true;
                return Err(ApiError::OutcomeUnknown);
            }
        }
        if self
            .domain
            .put(
                &key,
                serde_json::to_value(&position).expect("typed reading position"),
            )
            .await
            .is_err()
        {
            self.state
                .lock()
                .expect("attention reading positions")
                .uncertain = true;
            return Err(ApiError::OutcomeUnknown);
        }
        let mut state = self.state.lock().expect("attention reading positions");
        for oldest in &evicted {
            state.positions.remove(oldest);
        }
        state
            .recency
            .retain(|entry| entry != &key && !evicted.contains(entry));
        state.recency.push_back(key.clone());
        state.positions.insert(key, position.clone());
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
            .requiring_local::<SessionContract>()
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
        for (key, value) in domain.snapshot().await {
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
            session: plan.local::<SessionContract>()?,
            external: plan.local::<ExternalConversationsContract>()?,
            epoch: plan
                .local::<ConnectionDescriptionContract>()?
                .host_epoch
                .clone(),
            domain,
            state: Mutex::new(State {
                closed: false,
                uncertain: false,
                recency: positions.keys().cloned().collect(),
                positions,
            }),
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
