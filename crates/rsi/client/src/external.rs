use futures_util::future::BoxFuture;
use rsi_acp_protocol::{
    observation::{ConversationId, Record, RecordKind},
    service::{Error, ExternalConversations, Result, Setup, View},
};
use rsi_conversation::{ConversationCapabilities, ConversationIdentity, ExternalSource};
use rsi_meta_execution::Execution;
use serde::{Deserialize, Serialize};
use std::{
    collections::VecDeque,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tokio::sync::{Semaphore, watch};
use tokio_util::{sync::CancellationToken, task::TaskTracker};

/// One bounded human-readable observation, retaining an exact raw source.
#[derive(Clone, Debug, Serialize)]
pub struct ExternalBlock {
    /// Stable epoch and sequence identity, never a native Fact key.
    pub key: String,
    /// Human, assistant, reasoning, tool, permission or other update.
    pub role: String,
    /// Plain source text; the renderer must escape it normally.
    pub text: String,
    /// Exact journal source for full byte windows.
    pub source: ExternalSource,
    /// Whether the displayed text omits source bytes.
    pub truncated: bool,
}
/// Complete bounded external presentation; it confers no remote durability.
#[derive(Clone, Debug, Serialize)]
pub struct ExternalView {
    /// Distinct external identity.
    pub conversation: ConversationIdentity,
    /// Last successful Host observation and exact live permissions.
    pub observed: View,
    /// Capabilities of this connection and conversation kind.
    pub capabilities: ConversationCapabilities,
    /// Last 128 admitted display records within a 1 MiB text budget.
    pub blocks: VecDeque<ExternalBlock>,
    /// Whether polling follows new observed records.
    pub following: bool,
    /// Whether another bounded page remains after the displayed window.
    pub more: bool,
    /// A local operation is active.
    pub busy: bool,
    /// Last categorical read or command failure, never raw remote payload.
    pub diagnostic: Option<String>,
}
/// Closed external controls, with no commands, environment or ambient execution.
#[derive(Clone, Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ExternalCommand {
    /// One prompt; unknown delivery is never automatically retried.
    Submit {
        /// Bounded text input.
        text: String,
    },
    /// Cancel and await the Host's settlement policy.
    Cancel,
    /// Close the Host-owned peer, independently of detaching this controller.
    Close,
    /// Explicit reconnect using advertised resume or load support.
    Reconnect {
        /// New is invalid for an existing conversation.
        setup: Setup,
    },
    /// Exact displayed option, with no inferred or persisted local always-grant.
    Answer {
        /// Decimal connection generation.
        generation: String,
        /// Exact request identity.
        permission: String,
        /// Exact option identity.
        option: String,
    },
    /// Read the next bounded history page.
    Next,
    /// Show history from its first available record, pausing following.
    Beginning,
    /// Follow observations without reconnecting or sending a prompt.
    Live,
    /// Refresh current observation without a mutation retry.
    Refresh,
}
#[derive(Debug)]
struct State {
    view: ExternalView,
    after: u64,
    bytes: usize,
    revision: Arc<()>,
}
/// Detachable shared controller. Its retirement does not close an external peer.
#[derive(Debug)]
pub struct ExternalController {
    service: Arc<dyn ExternalConversations>,
    id: ConversationId,
    execution: Execution,
    state: Mutex<State>,
    read: tokio::sync::Mutex<()>,
    controls: Arc<Semaphore>,
    sources: Arc<Semaphore>,
    stop: CancellationToken,
    closed: AtomicBool,
    admission: Mutex<()>,
    tasks: TaskTracker,
    changed: watch::Sender<u64>,
}
impl ExternalController {
    /// Attaches a read-only observer first; the Host remains the peer owner.
    ///
    /// # Panics
    /// Panics if an earlier panic poisoned the observation state lock.
    pub async fn attach(
        service: Arc<dyn ExternalConversations>,
        execution: Execution,
        id: ConversationId,
    ) -> Result<Arc<Self>> {
        let observed = service.view(&id).await?;
        let capabilities =
            ConversationCapabilities::external(&observed.snapshot, observed.connected);
        let owner = Arc::new(Self {
            service,
            id: id.clone(),
            execution,
            state: Mutex::new(State {
                view: ExternalView {
                    conversation: ConversationIdentity::External(id),
                    observed,
                    capabilities,
                    blocks: VecDeque::new(),
                    following: true,
                    more: false,
                    busy: false,
                    diagnostic: None,
                },
                after: 0,
                bytes: 0,
                revision: Arc::new(()),
            }),
            read: tokio::sync::Mutex::new(()),
            controls: Arc::new(Semaphore::new(1)),
            sources: Arc::new(Semaphore::new(2)),
            stop: CancellationToken::new(),
            closed: AtomicBool::new(false),
            admission: Mutex::new(()),
            tasks: TaskTracker::new(),
            changed: watch::channel(0).0,
        });
        owner.refresh(true).await?;
        let polling = owner.clone();
        let mut wake = owner.changes();
        drop(owner.execution.spawn(owner.tasks.track_future(async move {
            let mut delay=Duration::from_millis(500);
            loop {
                tokio::select! {biased;()=polling.stop.cancelled()=>break,result=wake.changed()=>{if result.is_err(){break;}},()=polling.execution.sleep(delay)=>{}}
                let result=tokio::select! {biased;()=polling.stop.cancelled()=>break,result=polling.refresh(false)=>result};
                let changed=match result {Ok(changed)=>changed,Err(error)=>{polling.failure(error);false}};
                let state=polling.state.lock().expect("external polling");
                let active=state.view.busy || state.view.more || !state.view.observed.permissions.is_empty() || matches!(state.view.observed.snapshot.status,rsi_acp_protocol::observation::Status::Starting|rsi_acp_protocol::observation::Status::Loading|rsi_acp_protocol::observation::Status::Running);
                delay=if changed||active {Duration::from_millis(500)}else{(delay*2).min(Duration::from_secs(8))};
                wake.borrow_and_update();
            }
        })));
        Ok(owner)
    }
    fn publish(&self, state: &mut State) {
        state.revision = Arc::new(());
        self.changed
            .send_modify(|value| *value = value.saturating_add(1));
    }
    /// Captures the bounded current projection.
    ///
    /// # Panics
    /// Panics if another operation poisoned the projection lock.
    pub fn view(&self) -> ExternalView {
        self.state.lock().expect("external view").view.clone()
    }
    /// Captures metadata and at most `maximum` UTF-8 bytes from the transcript tail.
    ///
    /// # Panics
    /// Panics if another operation poisoned the projection lock.
    pub fn view_tail(&self, maximum: usize) -> ExternalView {
        let state = self.state.lock().expect("external view");
        let view = &state.view;
        let mut remaining = maximum;
        let mut blocks = VecDeque::new();
        for block in view.blocks.iter().rev() {
            let overhead = block.role.len() + block.key.len() + " · \n\n".len();
            if remaining <= overhead {
                break;
            }
            let start = block
                .text
                .ceil_char_boundary(block.text.len().saturating_sub(remaining - overhead));
            let tail = ExternalBlock {
                key: block.key.clone(),
                role: block.role.clone(),
                text: block.text[start..].to_owned(),
                source: block.source.clone(),
                truncated: block.truncated || start != 0,
            };
            remaining -= overhead + tail.text.len();
            blocks.push_front(tail);
            if start != 0 {
                break;
            }
        }
        ExternalView {
            conversation: view.conversation.clone(),
            observed: view.observed.clone(),
            capabilities: view.capabilities,
            blocks,
            following: view.following,
            more: view.more,
            busy: view.busy,
            diagnostic: view.diagnostic.clone(),
        }
    }
    /// Immutable revision token for frame caching.
    ///
    /// # Panics
    /// Panics if another operation poisoned the projection lock.
    pub fn revision(&self) -> Arc<()> {
        self.state.lock().expect("external view").revision.clone()
    }
    /// Coalesced changes for native and Worker renderers.
    pub fn changes(&self) -> watch::Receiver<u64> {
        self.changed.subscribe()
    }
    /// Exact identity retained by this controller.
    pub fn id(&self) -> &ConversationId {
        &self.id
    }
    fn failure(&self, error: Error) {
        let mut state = self.state.lock().expect("external view");
        let diagnostic = error.to_string();
        let disconnected = matches!(error, Error::Unknown | Error::Journal | Error::NotFound);
        if state.view.diagnostic.as_ref() == Some(&diagnostic)
            && (!disconnected
                || (!state.view.observed.connected && state.view.observed.permissions.is_empty()))
        {
            return;
        }
        state.view.diagnostic = Some(diagnostic);
        if disconnected {
            state.view.observed.connected = false;
            state.view.observed.permissions.clear();
            state.view.capabilities =
                ConversationCapabilities::external(&state.view.observed.snapshot, false);
        }
        self.publish(&mut state);
    }
    async fn refresh(&self, advance: bool) -> Result<bool> {
        let _read = self.read.lock().await;
        let observed = self.service.view(&self.id).await?;
        let (epoch, after, fetch) = {
            let state = self.state.lock().expect("external view");
            (
                observed.snapshot.epoch,
                if observed.snapshot.epoch == state.view.observed.snapshot.epoch {
                    state.after
                } else {
                    0
                },
                advance
                    || state.view.following
                    || observed.snapshot.epoch != state.view.observed.snapshot.epoch,
            )
        };
        let page = if fetch {
            Some(self.service.page(&self.id, epoch, after).await?)
        } else {
            None
        };
        let mut state = self.state.lock().expect("external view");
        let changed = state.view.observed != observed
            || state
                .view
                .diagnostic
                .as_deref()
                .is_some_and(|value| value != Error::Unknown.to_string())
            || page
                .as_ref()
                .is_some_and(|page| !page.records.is_empty() || page.has_more != state.view.more);
        if state.view.observed.snapshot.epoch != epoch {
            state.view.blocks.clear();
            state.bytes = 0;
            state.after = 0;
        }
        state.view.capabilities =
            ConversationCapabilities::external(&observed.snapshot, observed.connected);
        state.view.observed = observed;
        // A successful observation clears read failure, but not an unknown send receipt.
        if state.view.diagnostic.as_deref() != Some(&Error::Unknown.to_string()) {
            state.view.diagnostic = None;
        }
        if let Some(page) = page {
            for record in page.records {
                if record.sequence <= state.after || record.epoch != epoch {
                    return Err(Error::Journal);
                }
                state.after = record.sequence;
                let block = block(&self.id, record)?;
                state.bytes += block.text.len();
                state.view.blocks.push_back(block);
                while state.view.blocks.len() > 128 || state.bytes > 1024 * 1024 {
                    let removed = state
                        .view
                        .blocks
                        .pop_front()
                        .expect("bounded external blocks");
                    state.bytes -= removed.text.len();
                }
            }
            state.view.more = page.has_more;
        }
        if changed {
            self.publish(&mut state);
        }
        Ok(changed)
    }
    /// Admits and retains one control even if its response waiter disappears.
    ///
    /// # Panics
    /// Panics if another operation poisoned an admission or projection lock.
    pub fn command(self: &Arc<Self>, command: ExternalCommand) -> BoxFuture<'static, Result<()>> {
        let _admission = self.admission.lock().expect("external admission");
        if self.closed.load(Ordering::Acquire) {
            return Box::pin(async { Err(Error::Stale) });
        }
        let Ok(permit) = self.controls.clone().try_acquire_owned() else {
            return Box::pin(async { Err(Error::Busy) });
        };
        {
            let mut state = self.state.lock().expect("external view");
            state.view.busy = true;
            self.publish(&mut state);
        }
        let owner = self.clone();
        let task=self.execution.spawn(self.tasks.track_future(async move {
            let _permit=permit;
            let result=tokio::select! {biased;()=owner.stop.cancelled()=>Err(Error::Unknown),result=owner.execute(command)=>result,()=owner.execution.sleep(Duration::from_secs(45))=>Err(Error::Unknown)};
            if let Err(error)=result {owner.failure(error);}
            {let mut state=owner.state.lock().expect("external view");state.view.busy=false;owner.publish(&mut state);}
            result
        }));
        Box::pin(async move { task.await.unwrap_or(Err(Error::Unknown)) })
    }
    async fn execute(&self, command: ExternalCommand) -> Result<()> {
        match command {
            ExternalCommand::Submit { text } => {
                self.service.submit(&self.id, &text).await?;
            }
            ExternalCommand::Cancel => {
                self.service.cancel(&self.id).await?;
            }
            ExternalCommand::Close => {
                self.service.close(&self.id).await?;
            }
            ExternalCommand::Reconnect { setup } => {
                self.service.reconnect(&self.id, setup).await?;
            }
            ExternalCommand::Answer {
                generation,
                permission,
                option,
            } => {
                let gen_number = generation.parse::<u64>().map_err(|_| Error::Input)?;
                {
                    let state = self.state.lock().expect("external view");
                    if gen_number.to_string() != generation
                        || !state.view.observed.permissions.iter().any(|pending| {
                            pending.generation == generation
                                && pending.id == permission
                                && pending.options.iter().any(|value| value.id == option)
                        })
                    {
                        return Err(Error::Stale);
                    }
                }
                self.service
                    .answer(&self.id, gen_number, &permission, &option)
                    .await?;
            }
            ExternalCommand::Beginning | ExternalCommand::Live => {
                let _read = self.read.lock().await;
                let mut state = self.state.lock().expect("external view");
                state.view.following = matches!(command, ExternalCommand::Live);
                state.view.blocks.clear();
                state.bytes = 0;
                state.after = 0;
            }
            ExternalCommand::Next | ExternalCommand::Refresh => {}
        }
        self.refresh(true).await.map(|_| ())
    }
    /// Rereads exact source kind and identity before returning one raw JSON byte window.
    pub async fn source(&self, source: &ExternalSource, start: usize) -> Result<Vec<u8>> {
        if self.closed.load(Ordering::Acquire) || source.conversation() != &self.id {
            return Err(Error::Stale);
        }
        let _permit = self.sources.try_acquire().map_err(|_| Error::Busy)?;
        let page = self
            .service
            .page(&self.id, source.epoch(), source.sequence() - 1)
            .await?;
        if !page.records.first().is_some_and(|record| {
            record.sequence == source.sequence()
                && record.epoch == source.epoch()
                && record.kind == source.kind()
        }) {
            return Err(Error::NotFound);
        }
        self.service
            .window(&self.id, source.epoch(), source.sequence(), start)
            .await
    }
    /// Detaches observations and control waiters; admitted Host mutations continue.
    ///
    /// # Panics
    /// Panics if another operation poisoned the admission lock.
    pub async fn retire(&self) {
        {
            let _admission = self.admission.lock().expect("external admission");
            self.closed.store(true, Ordering::Release);
            self.stop.cancel();
            self.tasks.close();
        }
        self.tasks.wait().await;
    }
}
fn block(id: &ConversationId, record: Record) -> Result<ExternalBlock> {
    let source = ExternalSource::new(id.clone(), record.epoch, record.sequence, record.kind)
        .map_err(|_| Error::Journal)?;
    let Some(value) = record.value else {
        return Ok(ExternalBlock {
            key: format!("{}:{}", record.epoch, record.sequence),
            role: record.kind.name().into(),
            text: "Large observation · open source to read".into(),
            source,
            truncated: true,
        });
    };
    let (role, text) = match record.kind {
        RecordKind::User => (
            "Human".into(),
            value
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|part| part["text"].as_str())
                .collect::<Vec<_>>()
                .join("\n"),
        ),
        RecordKind::Permission => (
            "Permission".into(),
            value["toolCall"]["title"]
                .as_str()
                .unwrap_or("External tool permission")
                .into(),
        ),
        RecordKind::Update => {
            let kind = value["sessionUpdate"].as_str().unwrap_or("update");
            let role = match kind {
                "agent_message_chunk" => "Assistant",
                "agent_thought_chunk" => "Reasoning",
                "user_message_chunk" => "Human",
                "tool_call" | "tool_call_update" => "Tool",
                _ => "Update",
            };
            (
                role.into(),
                value["content"]["text"].as_str().map_or_else(
                    || serde_json::to_string(&value).expect("bounded observed JSON"),
                    str::to_owned,
                ),
            )
        }
    };
    let mut text: String = text;
    let truncated = text.len() > 64 * 1024;
    if truncated {
        let mut end = 64 * 1024;
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        text.truncate(end);
    }
    Ok(ExternalBlock {
        key: format!("{}:{}", record.epoch, record.sequence),
        role,
        text,
        source,
        truncated,
    })
}
