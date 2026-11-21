use super::admission::Admission;
use super::{
    Arc, BTreeMap, ConfigValue, Deserialize, Mutex, PluginFactory, PreparedActivation, Result,
    activation, session_error,
};
use async_trait::async_trait;
use rsi_acp_protocol::{
    observation::Status as ExternalStatus,
    service::{ExternalConversations, ExternalConversationsContract},
};
use rsi_agent_turn_protocol::SessionActivityEvent;
use rsi_api_protocol::{
    ApiError, ApiRegistrarContract, CallOrigin, ConnectionDescriptionContract, HostEpoch,
    json_handler, measure_json,
};
use rsi_conversation::ConversationIdentity;
use rsi_meta::{ActivationPlan, EmitEventHandler};
use rsi_navigation_api::{
    attention::{
        Entry, MAXIMUM_ENTRIES, MAXIMUM_PAGE_BYTES, Mark, Operation, Page, Position, ReadAction,
        Reading, Status, Target,
    },
    revision,
};
use rsi_session_protocol::{ActivityStatus, SessionIngress, SessionIngressContract};

#[derive(Debug, Default, Clone, Copy, Eq, PartialEq)]
struct Record {
    epoch: u64,
    completed: u64,
    read: u64,
    manual: bool,
    failed: bool,
}

impl Record {
    fn reading(self) -> Reading {
        Reading {
            unseen: self.completed > self.read,
            manual: self.manual,
            failed: self.failed,
        }
    }

    fn mark(&mut self, action: ReadAction, through: u64) {
        if action != ReadAction::Unread {
            self.read = self.read.max(through);
        }
        if action != ReadAction::Acknowledge {
            self.manual = action == ReadAction::Unread;
        }
    }
}

#[derive(Default, Debug)]
struct Records {
    rows: Mutex<BTreeMap<ConversationIdentity, Record>>,
    changes: tokio::sync::watch::Sender<()>,
}

impl EmitEventHandler<SessionActivityEvent> for Records {
    fn handle(&self, event: &SessionActivityEvent) {
        match event {
            SessionActivityEvent::TurnStarted => {
                self.changes.send_replace(());
            }
            SessionActivityEvent::TurnEnded {
                session,
                sequence,
                failed,
            } => {
                let mut records = self.rows.lock().expect("reading records");
                let record = records
                    .entry(ConversationIdentity::Native(session.clone()))
                    .or_default();
                if *sequence > record.completed {
                    record.completed = *sequence;
                    record.failed = *failed;
                    self.changes.send_replace(());
                }
            }
            SessionActivityEvent::Retired(session) => {
                if self
                    .rows
                    .lock()
                    .expect("reading records")
                    .remove(&ConversationIdentity::Native(session.clone()))
                    .is_some()
                {
                    self.changes.send_replace(());
                }
            }
        }
    }
}

#[derive(Debug)]
struct Attention {
    session: Arc<dyn SessionIngress>,
    external: Arc<dyn ExternalConversations>,
    epoch: HostEpoch,
    records: Arc<Records>,
    admission: Arc<Admission>,
}

fn native_entry(
    id: ConversationIdentity,
    record: Record,
    status: Status,
    targets: Vec<Target>,
) -> Entry {
    Entry {
        position: Position {
            conversation: id,
            epoch: "0".into(),
            sequence: record.completed.to_string(),
        },
        reading: record.reading(),
        status,
        targets,
    }
}

impl Attention {
    async fn read(&self, origin: &CallOrigin) -> Result<Page> {
        let scoped = self.session.scoped(origin.clone());
        let native = scoped.activity().await.map_err(session_error)?;
        let external = self
            .external
            .residents()
            .await
            .map_err(|_| ApiError::Unavailable)?;
        let mut truncated = native.truncated;
        let mut entries = Vec::new();
        let managed = {
            let records = self.records.rows.lock().expect("reading records");
            let mut managed = std::collections::BTreeSet::new();
            for row in native.entries {
                let id = ConversationIdentity::Native(row.session);
                let record = records.get(&id).copied().unwrap_or_default();
                managed.insert(id.clone());
                let status = if !row.requests.is_empty() {
                    Status::Waiting
                } else {
                    match row.status {
                        ActivityStatus::Running => Status::Running,
                        ActivityStatus::Unknown => Status::Unknown,
                        ActivityStatus::Idle if record.reading().unread() => Status::Unread,
                        ActivityStatus::Idle => continue,
                    }
                };
                entries.push(native_entry(
                    id,
                    record,
                    status,
                    row.requests
                        .into_iter()
                        .map(|request| Target::Native { request })
                        .collect(),
                ));
            }
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
                let id = ConversationIdentity::External(row.snapshot.id);
                let epoch = row.snapshot.epoch;
                let sequence = revision(&row.sequence)?;
                let saved = records.get(&id).filter(|record| record.epoch == epoch);
                let reading = Reading {
                    manual: saved.is_some_and(|record| record.manual),
                    unseen: sequence > saved.map_or(0, |record| record.read),
                    failed: row.failed,
                };
                if status == Status::Unread && !reading.unread() {
                    continue;
                }
                entries.push(Entry {
                    reading,
                    position: Position {
                        conversation: id,
                        epoch: epoch.to_string(),
                        sequence: row.sequence,
                    },
                    status,
                    targets,
                });
            }

            managed
        };
        let mut after = None;
        let mut cold_count = 0;
        loop {
            use std::ops::Bound::{Excluded, Unbounded};
            let batch: Vec<_> = self
                .records
                .rows
                .lock()
                .expect("reading records")
                .range((after.as_ref().map_or(Unbounded, Excluded), Unbounded))
                .filter_map(|(id, record)| match id {
                    ConversationIdentity::Native(session)
                        if record.reading().unread() && !managed.contains(id) =>
                    {
                        Some(session.clone())
                    }
                    _ => None,
                })
                .take(64)
                .collect();
            if batch.is_empty() {
                break;
            }
            let visible = scoped
                .visible_activity_sessions(&batch)
                .await
                .map_err(session_error)?;
            let records = self.records.rows.lock().expect("reading records");
            for session in visible {
                let id = ConversationIdentity::Native(session);
                if let Some(record) = records.get(&id).filter(|r| r.reading().unread()) {
                    entries.push(native_entry(id, *record, Status::Unread, Vec::new()));
                    cold_count += 1;
                }
            }
            if batch.len() < 64 || cold_count > MAXIMUM_ENTRIES {
                break;
            }
            after = batch
                .into_iter()
                .next_back()
                .map(ConversationIdentity::Native);
        }
        entries.sort_by(|a, b| {
            (&a.status, &a.position.conversation).cmp(&(&b.status, &b.position.conversation))
        });
        if entries.len() > MAXIMUM_ENTRIES {
            truncated = true;
            entries.truncate(MAXIMUM_ENTRIES);
        }
        let mut remaining = MAXIMUM_PAGE_BYTES - 1024;
        if let Some(end) =
            entries
                .iter()
                .position(|row| match measure_json(row, remaining.saturating_sub(1)) {
                    Ok(bytes) => {
                        remaining -= bytes + 1;
                        false
                    }
                    Err(_) => true,
                })
        {
            entries.truncate(end);
            truncated = true;
        }
        super::finish_scope(origin, &[])?;
        Ok(Page {
            host_epoch: self.epoch.clone(),
            entries,
            truncated,
        })
    }

    async fn mark(&self, origin: CallOrigin, request: Mark) -> Result<Position> {
        request.position.validate()?;
        if request.host_epoch != self.epoch {
            return Err(ApiError::Invalid("attention Host changed".into()));
        }
        let mut position = request.position;
        let through = revision(&position.sequence)?;
        let epoch = revision(&position.epoch)?;
        let current = match &position.conversation {
            ConversationIdentity::Native(id) => {
                self.session
                    .scoped(origin.clone())
                    .read_summary(id)
                    .await
                    .map_err(session_error)?
                    .durable_fact_seq
            }
            ConversationIdentity::External(id) => {
                let row = self
                    .external
                    .residents()
                    .await
                    .map_err(|_| ApiError::Unavailable)?
                    .into_iter()
                    .find(|row| &row.snapshot.id == id && row.snapshot.epoch == epoch)
                    .ok_or_else(|| ApiError::Invalid("attention source changed".into()))?;
                revision(&row.sequence)?
            }
        };
        super::finish_scope(&origin, &[])?;
        let mut records = self.records.rows.lock().expect("reading records");
        let previous = records.get(&position.conversation).copied();
        if through > current
            || matches!(position.conversation, ConversationIdentity::Native(_))
                && through > previous.map_or(0, |r| r.completed)
        {
            return Err(ApiError::Invalid(
                "attention position is in the future".into(),
            ));
        }
        let mut record = previous.filter(|r| r.epoch == epoch).unwrap_or(Record {
            epoch,
            ..Record::default()
        });
        record.mark(request.action, through);
        let retained =
            (record.completed != 0 || record.read != 0 || record.manual).then_some(record);
        if previous != retained {
            if let Some(retained) = retained {
                records.insert(position.conversation.clone(), retained);
            } else {
                records.remove(&position.conversation);
            }
            self.records.changes.send_replace(());
        }
        position.sequence = through.max(record.read).to_string();
        Ok(position)
    }
}

#[derive(Debug)]
struct Follow {
    owner: Arc<Attention>,
    execution: rsi_meta::Execution,
}

#[async_trait]
impl rsi_api_protocol::ApiHandler for Follow {
    async fn invoke(
        &self,
        context: rsi_api_protocol::ApiContext,
        input: rsi_api_protocol::RetainedBytes,
        output: rsi_api_protocol::ApiResponseCapacity,
    ) -> Result<rsi_api_protocol::ApiOutput> {
        use rsi_api_protocol::{ApiMessage, ApiOutput, ApiResponseCapacity};
        serde_json::from_slice::<()>(input.as_bytes())
            .map_err(|_| ApiError::Invalid("invalid attention subscription".into()))?;
        let ApiResponseCapacity::Subscription { budget, maximum } = output else {
            return Err(ApiError::Backend(
                "attention subscription admission required".into(),
            ));
        };
        let owner = self.owner.clone();
        let execution = self.execution.clone();
        let mut changes = owner.records.changes.subscribe();
        Ok(ApiOutput::Stream(Box::pin(async_stream::try_stream! {
            let mut last = None;
            let mut seconds = 1;
            loop {
                let page = owner.read(&context.origin).await?;
                let changed = last.as_ref() != Some(&page);
                if changed {
                    let json = budget.encode(&page, maximum)?;
                    last = Some(page);
                    yield ApiMessage { json, binary: None };
                    seconds = 1;
                }
                // Other activity owners expose current state without a change signal.
                tokio::select! {
                    () = context.retiring.cancelled() => break,
                    result = changes.changed() => { if result.is_err() { break; } },
                    () = execution.sleep(std::time::Duration::from_secs(seconds)) => {
                        seconds = (seconds * 2).min(8);
                    },
                }
            }
        })))
    }
}

/// Host owner of shared, generation-local Session reminders.
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
        if !config.is_null() {
            return Err(rsi_meta::MetaError::InvalidInput(
                "attention accepts no configuration".into(),
            ));
        }
        Ok(PreparedActivation::new(ConfigValue::Null)
            .requiring_local::<SessionIngressContract>()
            .requiring_local::<ExternalConversationsContract>()
            .requiring_local::<ConnectionDescriptionContract>()
            .requiring_local::<ApiRegistrarContract>())
    }
    async fn activate(&self, plan: ActivationPlan) -> rsi_meta::Result<()> {
        let records = Arc::new(Records::default());
        plan.context()
            .on_emit::<SessionActivityEvent, _>(records.clone(), Default::default())?;
        let owner = Arc::new(Attention {
            session: plan.local::<SessionIngressContract>()?,
            external: plan.local::<ExternalConversationsContract>()?,
            epoch: plan
                .local::<ConnectionDescriptionContract>()?
                .host_epoch
                .clone(),
            records,
            admission: Admission::new(2, plan.context().runtime().execution().clone()),
        });
        let registrar = plan.local::<ApiRegistrarContract>()?;
        let read = owner.clone();
        let read = registrar
            .register(
                Operation::Read.spec(),
                json_handler(move |context, _: Empty| {
                    let read = read.clone();
                    async move {
                        read.admission
                            .clone()
                            .run_read(move || {
                                Box::pin(async move { read.read(&context.origin).await })
                            })?
                            .await
                            .map(Ok::<_, Never>)
                    }
                }),
            )
            .map_err(activation)?;
        let follow = registrar
            .register(
                Operation::Follow.spec(),
                Arc::new(Follow {
                    owner: owner.clone(),
                    execution: plan.context().runtime().execution().clone(),
                }),
            )
            .map_err(activation)?;
        let write = owner.clone();
        let write = registrar
            .register(
                Operation::Mark.spec(),
                json_handler(move |context, request: Mark| {
                    let write = write.clone();
                    async move {
                        write
                            .admission
                            .run_mutation(|| {
                                let write = write.clone();
                                Box::pin(async move { write.mark(context.origin, request).await })
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
                    futures_util::join!(
                        owner.admission.close(),
                        read.close(),
                        follow.close(),
                        write.close()
                    );
                    Ok(())
                })
            }),
        )
    }
}

#[cfg(test)]
#[path = "attention_tests.rs"]
mod tests;
