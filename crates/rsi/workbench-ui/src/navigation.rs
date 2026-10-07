use super::{
    ActivationPlan, Arc, BoxFuture, ConfigValue, LocalContract, Mutex, PluginFactory,
    PreparedActivation, Result, Work, contribute, error, meta, prepare, watch,
};
use async_trait::async_trait;
use futures_util::StreamExt;
use rsi_agent_session_protocol::SessionId;
use rsi_navigation_api::{
    ActivityCursor, NavigationClient, NavigationCursor, NavigationEntry, NavigationFilter,
    NavigationPage, OrderMembership, OrderScope, OrderSeed, PinnedEntry, SessionMetadata,
    SummaryRequest, WorkspaceFilter,
};
use serde::{Deserialize, Serialize};

/// Closed navigation actions; Store cursors remain private to this feature.
#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum NavigationCommand {
    /// Capture complete manual-order membership without reading transcripts.
    OrderSeed {
        /// Exact immutable coordinate scope, independent of search.
        scope: OrderScope,
    },
    /// Read a device-ordered page from the currently retained seed.
    OrderSummaries {
        /// Current seed ticket; stale controls cannot substitute membership.
        ticket: String,
        /// At most 64 exact members in desired order.
        sessions: Vec<SessionId>,
    },
    /// Acknowledge an explicitly displayed attention cut.
    MarkRead {
        /// Exact source identity and coordinates.
        position: rsi_navigation_api::attention::Position,
    },
    /// Expand a workspace group or continue its exact displayed page.
    Group {
        /// Registered or unregistered group identity.
        workspace: WorkspaceFilter,
        /// None begins a group; a ticket continues its last scanned cursor.
        ticket: Option<String>,
    },
    /// Release a collapsed group's page and cursor.
    CloseGroup {
        /// Exact group identity.
        workspace: WorkspaceFilter,
    },
    /// Begin a new query under a new view ticket.
    Query {
        /// Exact bounded filter.
        filter: NavigationFilter,
    },
    /// Continue the currently displayed query.
    Next {
        /// Current view ticket.
        ticket: String,
    },
    /// Edit one durable Session's navigation against the displayed metadata revision.
    Replace {
        /// Current view ticket.
        ticket: String,
        /// Durable Session identity.
        session: SessionId,
        /// Complete title/archive record.
        metadata: SessionMetadata,
    },
}
#[derive(Clone, Debug, Serialize)]
struct GroupPage {
    ticket: String,
    workspace: WorkspaceFilter,
    entries: Vec<NavigationEntry>,
    scanned: u32,
    more: bool,
    continued: bool,
    stale: bool,
    #[serde(skip)]
    after: Option<NavigationCursor>,
}
impl GroupPage {
    fn invalidate(&mut self) {
        self.stale = true;
        self.entries.clear();
        self.after = None;
        self.more = false;
        self.scanned = 0;
    }
}
fn group_key(workspace: &WorkspaceFilter) -> Result<String> {
    match workspace {
        WorkspaceFilter::Registered { id } => Ok(id.to_string()),
        WorkspaceFilter::Unregistered => Ok("other".into()),
        WorkspaceFilter::All => Err("Choose one workspace group".into()),
    }
}
#[derive(Clone, Debug, Serialize)]
struct OrderView {
    ticket: String,
    seed: OrderSeed,
    requested: Vec<SessionId>,
    entries: Vec<Option<NavigationEntry>>,
}
#[derive(Debug, Default, Serialize)]
struct View {
    order: Option<OrderView>,
    newer_activity: bool,
    #[serde(skip)]
    newest: Option<ActivityCursor>,
    #[serde(skip)]
    continued: bool,
    attention: Option<rsi_navigation_api::attention::Page>,
    attention_notice: Option<String>,
    ticket: String,
    filter: NavigationFilter,
    entries: Vec<NavigationEntry>,
    pins: Vec<PinnedEntry>,
    groups: std::collections::BTreeMap<String, GroupPage>,
    scanned: u32,
    more: bool,
    metadata_revision: String,
    diagnostic: Option<String>,
}
#[derive(Debug, Default)]
struct State {
    view: View,
    after: Option<NavigationCursor>,
}
/// Navigation view owner for one application connection.
#[derive(Debug)]
pub struct NavigationFeature {
    client: NavigationClient,
    attention: Option<rsi_navigation_api::attention::Client>,
    state: Mutex<State>,
    work: Work,
    serial: tokio::sync::Mutex<()>,
    background_read: Mutex<Option<tokio_util::sync::CancellationToken>>,
    refresh: watch::Sender<()>,
    stop: tokio_util::sync::CancellationToken,
}
impl NavigationFeature {
    async fn refresh_background(&self, refresh: bool) -> Option<bool> {
        let _serial = self.serial.lock().await;
        let cancel = tokio_util::sync::CancellationToken::new();
        {
            let mut active = self
                .background_read
                .lock()
                .expect("navigation background read poisoned");
            if self.work.slot.available_permits() == 0 || self.stop.is_cancelled() {
                return None;
            }
            *active = Some(cancel.clone());
        }
        let filter = self
            .state
            .lock()
            .expect("navigation view poisoned")
            .view
            .filter
            .clone();
        let result = tokio::select! { biased;
            () = cancel.cancelled() => None,
            () = self.stop.cancelled() => None,
            () = self.work.execution.sleep(std::time::Duration::from_secs(5)) => {
                self.state.lock().expect("navigation view poisoned").view.diagnostic =
                    Some("Automatic navigation refresh timed out; refresh when the service is available".into());
                Some(true)
            },
            changed = async {
                let continued = self.state.lock().expect("navigation state").view.continued;
                let changed = if refresh && !continued {
                    let _ = self.execute(NavigationCommand::Query { filter }).await;
                    true
                } else { self.observe_head(filter).await };
                self.refresh_attention().await || changed
            } => Some(refresh || changed),
        };
        self.background_read
            .lock()
            .expect("navigation background read poisoned")
            .take();
        result
    }
    /// Captures the bounded current document projection without exposing Store cursors.
    ///
    /// # Panics
    /// Panics if an earlier panic poisoned this owner's state lock.
    pub fn view(&self) -> serde_json::Value {
        serde_json::to_value(&self.state.lock().expect("navigation view poisoned").view)
            .expect("navigation view encoding")
    }
    /// Subscribes to complete projection changes.
    pub fn changes(&self) -> watch::Receiver<u64> {
        self.work.changed.subscribe()
    }
    /// Coalesces a confirmed durable publication into a read of the current filter.
    pub fn invalidate(&self) {
        if !self.stop.is_cancelled() {
            self.refresh.send_replace(());
        }
    }
    /// Runs one retained navigation action.
    ///
    /// # Panics
    /// Panics if an earlier panic poisoned this owner's state lock.
    pub fn command(self: &Arc<Self>, command: NavigationCommand) -> BoxFuture<'static, Result<()>> {
        let owner = self.clone();
        self.work.run(async move {
            if let Some(read) = owner
                .background_read
                .lock()
                .expect("navigation background read poisoned")
                .as_ref()
            {
                read.cancel();
            }
            let _serial = owner.serial.lock().await;
            owner.execute(command).await
        })
    }
    async fn execute(&self, command: NavigationCommand) -> Result<()> {
        self.state
            .lock()
            .expect("navigation view poisoned")
            .view
            .diagnostic = None;
        let mut saved = false;
        let result = self.execute_inner(command, &mut saved).await;
        if let Err(message) = &result {
            let mut state = self.state.lock().expect("navigation view poisoned");
            if saved {
                state.view.ticket.clear();
                state.view.entries.clear();
                state.view.pins.clear();
                state.after = None;
                state.view.more = false;
                state.view.scanned = 0;
                for group in state.view.groups.values_mut() {
                    group.invalidate();
                }
                state.view.diagnostic =
                    Some(format!("Change saved. Refresh unavailable: {message}"));
                return Ok(());
            }
            state.view.diagnostic = Some(message.clone());
        }
        result
    }
    async fn observe_head(&self, filter: NavigationFilter) -> bool {
        let Ok(page) = self.client.query(filter, None).await else {
            return false;
        };
        let mut state = self.state.lock().expect("navigation state");
        let newer = state.view.newest != page.newest;
        let changed = newer != state.view.newer_activity;
        state.view.newer_activity = newer;
        changed
    }
    async fn order_summaries(&self, ticket: &str, sessions: Vec<SessionId>) -> Result<()> {
        let seed = self
            .state
            .lock()
            .expect("navigation state")
            .view
            .order
            .as_ref()
            .filter(|view| view.ticket == ticket)
            .map(|view| view.seed.clone())
            .ok_or("Manual order changed; refresh complete membership")?;
        let OrderMembership::Available { members, groups } = &seed.membership else {
            return Err("Manual ordering exceeds complete membership bounds".into());
        };
        let selected = sessions
            .iter()
            .map(|id| {
                members
                    .binary_search_by(|member| member.session.cmp(id))
                    .map(|index| &members[index])
            })
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(|_| "Summary is outside the complete order membership")?;
        let request = SummaryRequest {
            sessions: sessions.clone(),
            metadata_revision: seed.metadata_revision.clone(),
        };
        let page = self.client.summaries(request).await.map_err(error)?;
        for (row, member) in page.entries.iter().zip(selected) {
            if let Some(row) = row {
                let coordinates = &groups[usize::from(member.group)];
                if row.location != *coordinates.location()
                    || row.path != coordinates.path()
                    || row.metadata.pinned != member.pinned
                    || row.metadata.archived != member.archived
                {
                    return Err("Summary changed order group or partition".into());
                }
            }
        }
        let mut state = self.state.lock().expect("navigation state");
        let view = state
            .view
            .order
            .as_mut()
            .filter(|view| view.ticket == ticket)
            .ok_or("Manual order changed")?;
        view.requested = sessions;
        view.entries = page.entries;
        Ok(())
    }
    async fn page(
        &self,
        filter: NavigationFilter,
        mut after: Option<NavigationCursor>,
    ) -> Result<(NavigationPage, u32)> {
        let mut scanned = 0;
        loop {
            let page = self
                .client
                .query(filter.clone(), after)
                .await
                .map_err(error)?;
            scanned += u32::from(page.scanned);
            if !page.entries.is_empty() || page.next.is_none() || scanned >= 4096 {
                return Ok((page, scanned));
            }
            after = page.next;
        }
    }
    async fn group(&self, workspace: WorkspaceFilter, ticket: Option<String>) -> Result<()> {
        let key = group_key(&workspace)?;
        let (mut filter, after) = {
            let state = self.state.lock().expect("navigation view poisoned");
            let after = if let Some(ticket) = &ticket {
                let group = state
                    .view
                    .groups
                    .get(&key)
                    .filter(|group| &group.ticket == ticket)
                    .ok_or("Workspace page changed; expand it again")?;
                if group.stale {
                    return Err("Conversations changed; refresh this workspace".into());
                }
                let Some(after) = group.after.clone() else {
                    return Ok(());
                };
                Some(after)
            } else {
                None
            };
            if !state.view.groups.contains_key(&key) && state.view.groups.len() >= 16 {
                return Err(
                    "Close a workspace group before opening another (16 group limit)".into(),
                );
            }
            (state.view.filter.clone(), after)
        };
        filter.workspace = workspace.clone();
        let (page, scanned) = self.page(filter, after).await?;
        let mut state = self.state.lock().expect("navigation view poisoned");
        if page.metadata_revision != state.view.metadata_revision {
            return Err("Navigation changed; refresh before continuing this workspace".into());
        }
        if ticket.is_none()
            && state.view.groups.get(&key).is_some_and(|previous| {
                !previous.stale
                    && !previous.continued
                    && previous.entries == page.entries
                    && previous.after == page.next
                    && previous.scanned == scanned
            })
        {
            return Ok(());
        }
        state.view.groups.insert(
            key,
            GroupPage {
                ticket: rsi_ui::fresh_identity("group")?,
                workspace,
                entries: page.entries,
                scanned,
                more: page.next.is_some(),
                continued: ticket.is_some(),
                stale: false,
                after: page.next,
            },
        );
        Ok(())
    }
    #[allow(clippy::too_many_lines)] // Closed command dispatcher shares one serialized navigation owner.
    async fn execute_inner(&self, command: NavigationCommand, saved: &mut bool) -> Result<()> {
        if let NavigationCommand::OrderSeed { scope } = command {
            let seed = self.client.order_seed(scope).await.map_err(error)?;
            let ticket = rsi_ui::fresh_identity("order")?;
            self.state.lock().expect("navigation state").view.order = Some(OrderView {
                ticket,
                seed,
                requested: Vec::new(),
                entries: Vec::new(),
            });
            return Ok(());
        }
        if let NavigationCommand::OrderSummaries { ticket, sessions } = command {
            return self.order_summaries(&ticket, sessions).await;
        }
        let continued = matches!(&command, NavigationCommand::Next { .. });
        if let NavigationCommand::Group { workspace, ticket } = command {
            return self.group(workspace, ticket).await;
        }
        if let NavigationCommand::CloseGroup { workspace } = command {
            self.state
                .lock()
                .expect("navigation view poisoned")
                .view
                .groups
                .remove(&group_key(&workspace)?);
            return Ok(());
        }
        if let NavigationCommand::MarkRead { position } = command {
            self.attention
                .as_ref()
                .ok_or("Attention navigation unavailable")?
                .mark_read(position)
                .await
                .map_err(error)?;
            self.refresh_attention().await;
            return Ok(());
        }
        let (filter, after) = match command {
            NavigationCommand::OrderSeed { .. }
            | NavigationCommand::OrderSummaries { .. }
            | NavigationCommand::MarkRead { .. }
            | NavigationCommand::Group { .. }
            | NavigationCommand::CloseGroup { .. } => unreachable!("handled above"),
            NavigationCommand::Query { filter } => {
                filter.validate().map_err(error)?;
                (filter, None)
            }
            NavigationCommand::Next { ticket } => {
                let state = self.state.lock().expect("navigation view poisoned");
                if state.view.ticket.is_empty() || state.view.ticket != ticket {
                    return Err("Navigation changed; use the current controls".into());
                }
                if state.after.is_none() {
                    return Ok(());
                }
                (state.view.filter.clone(), state.after.clone())
            }
            NavigationCommand::Replace {
                ticket,
                session,
                metadata,
            } => {
                let (revision, filter) = {
                    let state = self.state.lock().expect("navigation view poisoned");
                    if state.view.ticket.is_empty() || state.view.ticket != ticket {
                        return Err("Navigation changed; refresh before editing".into());
                    }
                    (
                        state.view.metadata_revision.clone(),
                        state.view.filter.clone(),
                    )
                };
                self.client
                    .replace(session, &revision, metadata)
                    .await
                    .map_err(error)?;
                *saved = true;
                (filter, None)
            }
        };
        let (page, scanned) = self.page(filter.clone(), after).await?;
        let pins = self.client.pinned(filter.clone()).await.map_err(error)?;
        if pins.metadata_revision != page.metadata_revision {
            return Err("Navigation changed during refresh; try again".into());
        }
        let groups = {
            let mut state = self.state.lock().expect("navigation view poisoned");
            let mut groups = if filter == state.view.filter {
                std::mem::take(&mut state.view.groups)
            } else {
                std::collections::BTreeMap::new()
            };
            let changed = page.metadata_revision != state.view.metadata_revision;
            if changed {
                for group in groups.values_mut() {
                    group.invalidate();
                }
            }
            let refresh = groups
                .values()
                .filter(|group| !group.stale && !group.continued)
                .map(|group| group.workspace.clone())
                .collect::<Vec<_>>();
            let ticket = if filter == state.view.filter
                && page.metadata_revision == state.view.metadata_revision
                && page.entries == state.view.entries
                && page.next == state.after
                && pins.entries == state.view.pins
                && scanned == state.view.scanned
                && !state.view.ticket.is_empty()
            {
                state.view.ticket.clone()
            } else {
                rsi_ui::fresh_identity("navigation")?
            };
            let order = state
                .view
                .order
                .take()
                .filter(|order| order.seed.metadata_revision == page.metadata_revision);
            let newer_activity = continued && state.view.newest != page.newest;
            let newest = if continued {
                state.view.newest.take()
            } else {
                page.newest
            };
            let attention = state.view.attention.take();
            let attention_notice = state.view.attention_notice.take();
            *state = State {
                view: View {
                    order,
                    newer_activity,
                    newest,
                    continued,
                    attention,
                    attention_notice,
                    ticket,
                    filter,
                    entries: page.entries,
                    pins: pins.entries,
                    groups,
                    scanned,
                    more: page.next.is_some(),
                    metadata_revision: page.metadata_revision,
                    diagnostic: None,
                },
                after: page.next,
            };
            refresh
        };
        let results = futures_util::stream::iter(groups)
            .map(|group| async move {
                let key = group_key(&group)?;
                let result = self.group(group, None).await;
                if result.is_err()
                    && let Some(group) = self
                        .state
                        .lock()
                        .expect("navigation view poisoned")
                        .view
                        .groups
                        .get_mut(&key)
                {
                    group.invalidate();
                }
                result
            })
            .buffer_unordered(4)
            .collect::<Vec<_>>()
            .await;
        if results.iter().any(Result::is_err) {
            return Err(
                "Some workspace groups could not refresh; use Refresh this workspace".into(),
            );
        }
        Ok(())
    }
    async fn refresh_attention(&self) -> bool {
        let Some(client) = &self.attention else {
            return false;
        };
        let result = client.read().await;
        let mut state = self.state.lock().expect("navigation attention view");
        let (page, notice) = match result {
            Ok(page) => (Some(page), None),
            Err(_) => (
                state.view.attention.clone(),
                Some("Activity refresh unavailable; showing the last observation".to_owned()),
            ),
        };
        let changed = state.view.attention != page || state.view.attention_notice != notice;
        state.view.attention = page;
        state.view.attention_notice = notice;
        changed
    }
}
/// Nominal per-application navigation presentation capability.
#[derive(Debug)]
pub struct NavigationFeatureContract;
impl LocalContract for NavigationFeatureContract {
    const KEY: &'static str = "rsi.workbench.navigation";
    type Service = NavigationFeature;
}
/// Ordinary navigation feature plugin.
#[derive(Clone, Debug, Default)]
pub struct NavigationFeatureFactory;
#[async_trait]
impl PluginFactory for NavigationFeatureFactory {
    fn prepare(&self, config: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        prepare(config)
    }
    async fn activate(&self, plan: ActivationPlan) -> rsi_meta::Result<()> {
        let (refresh, mut changes) = watch::channel(());
        let feature = Arc::new(NavigationFeature {
            attention: rsi_navigation_api::attention::Client::new(
                plan.local::<rsi_api_protocol::ApiClientContract>()?,
            )
            .ok(),
            client: NavigationClient::new(plan.local::<rsi_api_protocol::ApiClientContract>()?)
                .map_err(meta)?,
            state: Mutex::default(),
            work: Work::new(plan.context().runtime().execution().clone()),
            serial: tokio::sync::Mutex::new(()),
            background_read: Mutex::new(None),
            refresh,
            stop: tokio_util::sync::CancellationToken::new(),
        });
        let _initial = feature
            .command(NavigationCommand::Query {
                filter: NavigationFilter::default(),
            })
            .await;
        feature.refresh_attention().await;
        let supply = plan
            .context()
            .provide_local::<NavigationFeatureContract>(feature.clone())?;
        let background = feature.clone();
        drop(
            feature
                .work
                .execution
                .spawn(feature.work.tasks.track_future(async move {
                let work = async {
                    let mut polling = AttentionPolling::default();
                    let mut pending = false;
                    loop {
                        let refresh = tokio::select! {
                            result = changes.changed() => {if result.is_err() {break;} true},
                            () = background.work.execution.sleep(std::time::Duration::from_secs(polling.seconds)) => false,
                        };
                        let refresh = pending || refresh || changes.has_changed().unwrap_or(false);
                        acknowledge_refresh(&mut changes, refresh);
                        let refreshed = background.refresh_background(refresh).await;
                        pending = refresh && refreshed.is_none();
                        polling.observe(pending || refreshed.unwrap_or(false));
                        if refreshed == Some(true) { background
                            .work
                            .changed
                            .send_modify(|revision| *revision = revision.saturating_add(1)); }
                    }
                };
                tokio::select! { biased; () = background.stop.cancelled() => {}, () = work => {} }
            })),
        );
        plan.defer(
            "drain navigation feature",
            Box::new(move || {
                Box::pin(async move {
                    drop(supply);
                    feature.stop.cancel();
                    feature.work.close().await;
                    Ok(())
                })
            }),
        )?;
        contribute(
            &plan,
            "rsi.workbench.navigation",
            "Session navigation",
            Arc::new(Card),
        )
    }
}
#[derive(Debug)]
struct Card;
impl rsi_ui::SurfaceRenderer for Card {
    fn render(&self, target: &rsi_meta::Context) -> rsi_ui::Result<rsi_ui::UiView> {
        let feature = target
            .lookup_local::<NavigationFeatureContract>()
            .ok_or(rsi_ui::UiError::Retired)?;
        let state = feature.state.lock().expect("navigation view poisoned");
        Ok(rsi_ui::UiView {
            title: "Session navigation".into(),
            elements: vec![
                rsi_ui::UiElement::Field {
                    label: "Matches on this page".into(),
                    value: state.view.entries.len().to_string(),
                },
                rsi_ui::UiElement::Field {
                    label: "Rows scanned".into(),
                    value: state.view.scanned.to_string(),
                },
            ],
        })
    }
}

fn acknowledge_refresh(changes: &mut watch::Receiver<()>, refresh: bool) {
    if refresh {
        changes.borrow_and_update();
    }
}
struct AttentionPolling {
    seconds: u64,
}
impl Default for AttentionPolling {
    fn default() -> Self {
        Self { seconds: 1 }
    }
}
impl AttentionPolling {
    fn observe(&mut self, changed: bool) {
        self.seconds = if changed {
            1
        } else {
            (self.seconds * 2).min(8)
        };
    }
}
#[cfg(test)]
mod polling_tests {
    use super::AttentionPolling;
    #[test]
    fn publication_after_an_idle_poll_decision_is_not_consumed() {
        let (send, mut changes) = tokio::sync::watch::channel(());
        let refresh = changes.has_changed().unwrap();
        send.send_replace(());
        super::acknowledge_refresh(&mut changes, refresh);
        assert!(changes.has_changed().unwrap());
        super::acknowledge_refresh(&mut changes, true);
        assert!(!changes.has_changed().unwrap());
    }
    #[test]
    fn unchanged_attention_backs_off_and_invalidation_restores_responsiveness() {
        let mut polling = AttentionPolling::default();
        for expected in [2, 4, 8, 8] {
            polling.observe(false);
            assert_eq!(polling.seconds, expected);
        }
        polling.observe(true);
        assert_eq!(polling.seconds, 1);
    }
}

#[cfg(test)]
mod group_tests {
    use super::*;
    use rsi_api_protocol::*;
    use rsi_navigation_api::NavigationOperation;
    use serde_json::{Value, json};
    #[derive(Debug)]
    struct Pages {
        block_next_read: std::sync::atomic::AtomicBool,
        read_entered: tokio::sync::Notify,
        release_read: tokio::sync::Notify,
        revision: std::sync::atomic::AtomicU64,
        other_reads: std::sync::atomic::AtomicUsize,
        queries: std::sync::atomic::AtomicUsize,
        replacements: std::sync::atomic::AtomicUsize,
        race_pins: std::sync::atomic::AtomicBool,
        fail_registered: std::sync::atomic::AtomicBool,
        description: ConnectionDescription,
        operations: Vec<OperationSpec>,
    }
    #[async_trait]
    impl ApiClient for Pages {
        fn description(&self) -> &ConnectionDescription {
            &self.description
        }
        fn operations(&self) -> &[OperationSpec] {
            &self.operations
        }
        fn input_budget(&self, _: OperationClass) -> ByteBudget {
            ByteBudget::default()
        }
        async fn call(
            &self,
            operation: &OperationSpec,
            input: RetainedBytes,
        ) -> rsi_api_protocol::Result<ApiOutput> {
            if self
                .block_next_read
                .swap(false, std::sync::atomic::Ordering::SeqCst)
            {
                self.read_entered.notify_one();
                self.release_read.notified().await;
            }
            if operation == &NavigationOperation::Query.spec() {
                self.queries
                    .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            }
            let request: Value = serde_json::from_slice(input.as_bytes()).unwrap();
            let revision = self
                .revision
                .load(std::sync::atomic::Ordering::SeqCst)
                .to_string();
            let page = if operation == &NavigationOperation::Replace.spec() {
                self.replacements
                    .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                let revision = self
                    .revision
                    .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
                    + 1;
                json!({"revision":revision.to_string(),"session":request["session"],"metadata":request["metadata"]})
            } else if operation == &NavigationOperation::OrderSeed.spec() {
                json!({"scope":request,"host_epoch":self.description.host_epoch,"metadata_revision":revision,"membership":{
                    "kind":"available","groups":[{"location":{"kind":"local"},"path":"/project"}],
                    "members":[{"session":"one","group":0,"last_activity_ms":"1","pinned":false,"archived":false},{"session":"two","group":0,"last_activity_ms":"2","pinned":false,"archived":false}]
                }})
            } else if operation == &NavigationOperation::Summaries.spec() {
                if request["metadata_revision"] != revision {
                    return Err(ApiError::Invalid("metadata changed".into()));
                }
                let entries = request["sessions"].as_array().unwrap().iter().map(|id|json!({"session":id,"created_at_ms":"1","last_activity_ms":"2","location":{"kind":"local"},"path":"/project","workspace":null,"metadata":{"pinned":false,"archived":false,"title":null}})).collect::<Vec<_>>();
                json!({"metadata_revision":revision,"entries":entries})
            } else if operation == &NavigationOperation::Pinned.spec() {
                let revision = if self.race_pins.load(std::sync::atomic::Ordering::SeqCst) {
                    "999".to_owned()
                } else {
                    revision
                };
                json!({"metadata_revision":revision,"entries":[]})
            } else {
                let filter = &request["filter"];
                let group = filter["workspace"]["kind"] != "all";
                if filter["workspace"]["kind"] == "registered"
                    && self
                        .fail_registered
                        .load(std::sync::atomic::Ordering::SeqCst)
                {
                    return Err(ApiError::Unavailable);
                }
                if filter["workspace"]["kind"] == "unregistered" {
                    self.other_reads
                        .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                }
                let before = request["after"]["token"]
                    .as_str()
                    .and_then(|token| u64::from_str_radix(token, 16).ok())
                    .unwrap_or(10000);
                let next=(group&&before>5904).then(||json!({"filter":filter,"host_epoch":self.description.host_epoch,"metadata_revision":revision,"after":request["after"]["after"],"token":format!("{:032x}",before-256)}));
                json!({"metadata_revision":revision,"newest":if group {Some(json!({"last_activity_ms":10000,"session_id":"row-10000"}))} else {None},"entries":[],"scanned":if group {256}else{0},"next":next})
            };
            Ok(ApiOutput::Reply(ApiMessage {
                json: ByteBudget::default().encode(&page, 2 * 1024 * 1024)?,
                binary: None,
            }))
        }
    }
    fn fixture() -> (Arc<Pages>, Arc<NavigationFeature>) {
        let api = Arc::new(Pages {
            block_next_read: false.into(),
            read_entered: tokio::sync::Notify::new(),
            release_read: tokio::sync::Notify::new(),
            revision: 1.into(),
            other_reads: 0.into(),
            queries: 0.into(),
            replacements: 0.into(),
            race_pins: false.into(),
            fail_registered: false.into(),
            description: ConnectionDescription {
                wire_version: 1,
                endpoint_id: EndpointId::from_bytes([1; 16]),
                host_epoch: HostEpoch::from_bytes([2; 16]),
            },
            operations: [
                NavigationOperation::Query,
                NavigationOperation::Pinned,
                NavigationOperation::Replace,
                NavigationOperation::OrderSeed,
                NavigationOperation::Summaries,
            ]
            .map(NavigationOperation::spec)
            .into(),
        });
        let owner = Arc::new(NavigationFeature {
            client: NavigationClient::new(api.clone()).unwrap(),
            attention: None,
            state: Mutex::default(),
            work: Work::new(rsi_meta::Execution::native(
                tokio::runtime::Handle::current(),
            )),
            serial: tokio::sync::Mutex::new(()),
            background_read: Mutex::new(None),
            refresh: watch::channel(()).0,
            stop: tokio_util::sync::CancellationToken::new(),
        });
        (api, owner)
    }
    #[tokio::test]
    async fn full_background_refresh_does_not_issue_a_second_query_for_its_head() {
        let (api, owner) = fixture();
        assert_eq!(owner.refresh_background(true).await, Some(true));
        assert_eq!(api.queries.load(std::sync::atomic::Ordering::SeqCst), 1);
        let _ = owner.refresh_background(false).await;
        assert_eq!(api.queries.load(std::sync::atomic::Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn manual_summary_commands_keep_seed_identity_order_and_metadata_revision() {
        let (api, owner) = fixture();
        owner
            .command(NavigationCommand::Query {
                filter: NavigationFilter::default(),
            })
            .await
            .unwrap();
        owner
            .command(NavigationCommand::OrderSeed {
                scope: OrderScope::All,
            })
            .await
            .unwrap();
        let ticket = owner.view()["order"]["ticket"].as_str().unwrap().to_owned();
        let ids = ["two", "one"]
            .map(|id| SessionId::new(id).unwrap())
            .to_vec();
        owner
            .command(NavigationCommand::OrderSummaries {
                ticket: ticket.clone(),
                sessions: ids.clone(),
            })
            .await
            .unwrap();
        assert_eq!(owner.view()["order"]["entries"][0]["session"], "two");
        for sessions in [
            vec![SessionId::new("foreign").unwrap()],
            vec![ids[0].clone(), ids[0].clone()],
        ] {
            assert!(
                owner
                    .command(NavigationCommand::OrderSummaries {
                        ticket: ticket.clone(),
                        sessions
                    })
                    .await
                    .is_err()
            );
        }
        assert!(
            owner
                .command(NavigationCommand::OrderSummaries {
                    ticket: "stale".into(),
                    sessions: ids.clone()
                })
                .await
                .is_err()
        );
        api.revision.store(2, std::sync::atomic::Ordering::SeqCst);
        assert!(
            owner
                .command(NavigationCommand::OrderSummaries {
                    ticket,
                    sessions: ids
                })
                .await
                .is_err()
        );
        owner
            .command(NavigationCommand::Query {
                filter: NavigationFilter::default(),
            })
            .await
            .unwrap();
        assert!(owner.view()["order"].is_null());
        owner.work.close().await;
    }
    #[tokio::test]
    async fn user_action_preempts_a_blocked_automatic_refresh() {
        let (api, owner) = fixture();
        owner
            .command(NavigationCommand::Query {
                filter: NavigationFilter::default(),
            })
            .await
            .unwrap();
        api.block_next_read
            .store(true, std::sync::atomic::Ordering::SeqCst);
        let background = {
            let owner = owner.clone();
            tokio::spawn(async move { owner.refresh_background(true).await })
        };
        api.read_entered.notified().await;
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            owner.command(NavigationCommand::CloseGroup {
                workspace: WorkspaceFilter::Unregistered,
            }),
        )
        .await;
        api.release_read.notify_one();
        let refreshed = background.await.unwrap();
        result.unwrap().unwrap();
        assert_eq!(
            refreshed, None,
            "foreground admission cancels automatic reads"
        );
        owner.work.close().await;
    }

    #[tokio::test]
    async fn identical_group_refresh_preserves_its_ticket() {
        let (_, owner) = fixture();
        owner
            .command(NavigationCommand::Query {
                filter: NavigationFilter::default(),
            })
            .await
            .unwrap();
        owner
            .command(NavigationCommand::Group {
                workspace: WorkspaceFilter::Unregistered,
                ticket: None,
            })
            .await
            .unwrap();
        let first = owner.view()["groups"]["other"].clone();
        owner
            .command(NavigationCommand::Query {
                filter: NavigationFilter::default(),
            })
            .await
            .unwrap();
        assert_eq!(owner.view()["groups"]["other"], first);
        owner.work.close().await;
    }

    #[tokio::test(start_paused = true)]
    async fn automatic_refresh_deadline_releases_reads_and_preserves_user_admission() {
        let (api, owner) = fixture();
        api.block_next_read
            .store(true, std::sync::atomic::Ordering::SeqCst);
        let background = {
            let owner = owner.clone();
            tokio::spawn(async move { owner.refresh_background(true).await })
        };
        api.read_entered.notified().await;
        tokio::time::advance(std::time::Duration::from_secs(5)).await;
        assert_eq!(background.await.unwrap(), Some(true));
        assert!(
            owner.view()["diagnostic"]
                .as_str()
                .unwrap()
                .contains("timed out")
        );
        assert!(owner.background_read.lock().unwrap().is_none());
        owner
            .command(NavigationCommand::Query {
                filter: NavigationFilter::default(),
            })
            .await
            .unwrap();
        assert!(owner.view()["diagnostic"].is_null());
        owner.work.close().await;
    }

    #[tokio::test]
    async fn committed_replacement_survives_refresh_revision_race_without_retry() {
        let (api, owner) = fixture();
        owner
            .command(NavigationCommand::Query {
                filter: NavigationFilter::default(),
            })
            .await
            .unwrap();
        let ticket = owner.view()["ticket"].as_str().unwrap().to_owned();
        owner.state.lock().unwrap().view.scanned = 17;
        api.race_pins
            .store(true, std::sync::atomic::Ordering::SeqCst);
        owner
            .command(NavigationCommand::Replace {
                ticket: ticket.clone(),
                session: SessionId::new("edited").unwrap(),
                metadata: SessionMetadata::default(),
            })
            .await
            .unwrap();
        assert_eq!(
            api.replacements.load(std::sync::atomic::Ordering::SeqCst),
            1
        );
        assert!(
            owner.view()["diagnostic"]
                .as_str()
                .unwrap()
                .starts_with("Change saved.")
        );
        assert_eq!(owner.view()["ticket"], "");
        assert_eq!(owner.view()["scanned"], 0);
        assert!(
            owner
                .command(NavigationCommand::Replace {
                    ticket,
                    session: SessionId::new("edited").unwrap(),
                    metadata: SessionMetadata::default()
                })
                .await
                .is_err()
        );
        assert_eq!(
            api.replacements.load(std::sync::atomic::Ordering::SeqCst),
            1
        );
        api.race_pins
            .store(false, std::sync::atomic::Ordering::SeqCst);
        owner
            .command(NavigationCommand::Query {
                filter: NavigationFilter::default(),
            })
            .await
            .unwrap();
        assert!(owner.view()["diagnostic"].is_null());
        assert!(!owner.view()["ticket"].as_str().unwrap().is_empty());
    }

    #[tokio::test]
    async fn refresh_failures_leave_failed_groups_stale_and_do_not_abandon_other_groups() {
        let (api, owner) = fixture();
        owner
            .command(NavigationCommand::Query {
                filter: NavigationFilter::default(),
            })
            .await
            .unwrap();
        for c in ['a', 'b'] {
            let workspace: WorkspaceFilter =
                serde_json::from_value(json!({"kind":"registered","id":c.to_string().repeat(64)}))
                    .unwrap();
            owner
                .command(NavigationCommand::Group {
                    workspace,
                    ticket: None,
                })
                .await
                .unwrap();
        }
        owner
            .command(NavigationCommand::Group {
                workspace: WorkspaceFilter::Unregistered,
                ticket: None,
            })
            .await
            .unwrap();
        let before = api.other_reads.load(std::sync::atomic::Ordering::SeqCst);
        api.fail_registered
            .store(true, std::sync::atomic::Ordering::SeqCst);
        assert!(
            owner
                .command(NavigationCommand::Query {
                    filter: NavigationFilter::default()
                })
                .await
                .is_err()
        );
        for c in ['a', 'b'] {
            let group = &owner.view()["groups"][c.to_string().repeat(64)];
            assert_eq!(group["stale"], true);
            assert_eq!(group["more"], false);
            assert_eq!(group["entries"], json!([]));
        }
        assert_eq!(owner.view()["groups"]["other"]["stale"], false);
        assert!(api.other_reads.load(std::sync::atomic::Ordering::SeqCst) > before);
    }

    #[tokio::test]
    async fn groups_keep_independent_empty_continuations_and_reject_replaced_tickets() {
        let (api, owner) = fixture();
        owner
            .command(NavigationCommand::Query {
                filter: NavigationFilter::default(),
            })
            .await
            .unwrap();
        let other = WorkspaceFilter::Unregistered;
        owner
            .command(NavigationCommand::Group {
                workspace: other.clone(),
                ticket: None,
            })
            .await
            .unwrap();
        let first = owner.view()["groups"]["other"].clone();
        assert_eq!(first["scanned"], 4096);
        assert_eq!(first["more"], true);
        assert_eq!(first["entries"], json!([]));
        assert!(first.get("after").is_none(), "Store cursors remain private");
        let registered: WorkspaceFilter =
            serde_json::from_value(json!({"kind":"registered","id":"a".repeat(64)})).unwrap();
        owner
            .command(NavigationCommand::Group {
                workspace: registered,
                ticket: None,
            })
            .await
            .unwrap();
        let untouched = owner.view()["groups"]["a".repeat(64)].clone();
        let ticket = first["ticket"].as_str().unwrap().to_owned();
        owner
            .command(NavigationCommand::Group {
                workspace: other.clone(),
                ticket: Some(ticket.clone()),
            })
            .await
            .unwrap();
        let next = owner.view();
        assert_eq!(next["groups"]["other"]["more"], false);
        assert_eq!(next["groups"]["other"]["continued"], true);
        assert_eq!(next["groups"]["a".repeat(64)], untouched);
        assert!(
            owner
                .command(NavigationCommand::Group {
                    workspace: other.clone(),
                    ticket: Some(ticket)
                })
                .await
                .is_err()
        );
        let retained = owner.view()["groups"]["other"].clone();
        let reads = api.other_reads.load(std::sync::atomic::Ordering::SeqCst);
        owner
            .command(NavigationCommand::Query {
                filter: NavigationFilter::default(),
            })
            .await
            .unwrap();
        assert_eq!(owner.view()["groups"]["other"], retained);
        assert_eq!(
            api.other_reads.load(std::sync::atomic::Ordering::SeqCst),
            reads,
            "refresh must not rescan a continued workspace"
        );
        api.revision.store(2, std::sync::atomic::Ordering::SeqCst);
        owner
            .command(NavigationCommand::Query {
                filter: NavigationFilter::default(),
            })
            .await
            .unwrap();
        assert_eq!(owner.view()["groups"]["other"]["stale"], true);
        assert_eq!(
            api.other_reads.load(std::sync::atomic::Ordering::SeqCst),
            reads,
            "metadata invalidation must not silently re-expand stale groups"
        );
        assert!(
            owner
                .command(NavigationCommand::Group {
                    workspace: other.clone(),
                    ticket: Some(retained["ticket"].as_str().unwrap().into())
                })
                .await
                .unwrap_err()
                .contains("refresh")
        );
        owner
            .command(NavigationCommand::Group {
                workspace: other,
                ticket: None,
            })
            .await
            .unwrap();
        assert_eq!(owner.view()["groups"]["other"]["stale"], false);
        assert_eq!(owner.view()["groups"]["other"]["continued"], false);
        owner.work.close().await;
    }
}
