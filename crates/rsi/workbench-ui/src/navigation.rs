use super::{
    ActivationPlan, Arc, BoxFuture, ConfigValue, LocalContract, Mutex, PluginFactory,
    PreparedActivation, Result, Work, contribute, error, meta, prepare, watch,
};
use async_trait::async_trait;
use rsi_agent_session_protocol::SessionId;
use rsi_navigation_api::{
    NavigationClient, NavigationCursor, NavigationEntry, NavigationFilter, SessionMetadata,
};
use serde::{Deserialize, Serialize};

/// Closed navigation actions; Store cursors remain private to this feature.
#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum NavigationCommand {
    /// Acknowledge an explicitly displayed attention cut.
    MarkRead {
        /// Exact source identity and coordinates.
        position: rsi_navigation_api::attention::Position,
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
#[derive(Debug, Default, Serialize)]
struct View {
    attention: Option<rsi_navigation_api::attention::Page>,
    attention_notice: Option<String>,
    ticket: String,
    filter: NavigationFilter,
    entries: Vec<NavigationEntry>,
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
    refresh: watch::Sender<()>,
    stop: tokio_util::sync::CancellationToken,
}
impl NavigationFeature {
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
            let result = owner.execute(command).await;
            owner
                .state
                .lock()
                .expect("navigation view poisoned")
                .view
                .diagnostic = result.as_ref().err().cloned();
            result
        })
    }
    async fn execute(&self, command: NavigationCommand) -> Result<()> {
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
        let (filter, mut after) = match command {
            NavigationCommand::MarkRead { .. } => unreachable!("handled above"),
            NavigationCommand::Query { filter } => {
                filter.validate().map_err(error)?;
                (filter, None)
            }
            NavigationCommand::Next { ticket } => {
                let state = self.state.lock().expect("navigation view poisoned");
                if state.view.ticket != ticket {
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
                    if state.view.ticket != ticket {
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
                (filter, None)
            }
        };
        let mut scanned = 0;
        let page = loop {
            let page = self
                .client
                .query(filter.clone(), after)
                .await
                .map_err(error)?;
            scanned += u32::from(page.scanned);
            if !page.entries.is_empty() || page.next.is_none() || scanned >= 4096 {
                break page;
            }
            after = page.next;
        };
        let mut state = self.state.lock().expect("navigation view poisoned");
        let attention = state.view.attention.take();
        let attention_notice = state.view.attention_notice.take();
        *state = State {
            view: View {
                attention,
                attention_notice,
                ticket: rsi_ui::fresh_identity("navigation")?,
                filter,
                entries: page.entries,
                scanned,
                more: page.next.is_some(),
                metadata_revision: page.metadata_revision,
                diagnostic: None,
            },
            after: page.next,
        };
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
                    loop {
                        let refresh = tokio::select! {
                            result = changes.changed() => {if result.is_err() {break;} true},
                            () = background.work.execution.sleep(std::time::Duration::from_secs(polling.seconds)) => false,
                        };
                        let Ok(_permit) = background.work.slot.acquire().await else {
                            break;
                        };
                        let refresh = refresh || changes.has_changed().unwrap_or(false);
                        acknowledge_refresh(&mut changes, refresh);
                        let filter = background
                            .state
                            .lock()
                            .expect("navigation view poisoned")
                            .view
                            .filter
                            .clone();
                        if refresh {
                            let result = background.execute(NavigationCommand::Query {filter}).await;
                            background.state.lock().expect("navigation view poisoned").view.diagnostic = result.err();
                        }
                        let attention_changed = background.refresh_attention().await;
                        polling.observe(refresh || attention_changed);
                        if refresh || attention_changed { background
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
