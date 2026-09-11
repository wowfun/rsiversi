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
        let (filter, mut after) = match command {
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
        *self.state.lock().expect("navigation view poisoned") = State {
            view: View {
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
                    while changes.changed().await.is_ok() {
                        let Ok(_permit) = background.work.slot.acquire().await else {
                            break;
                        };
                        changes.borrow_and_update();
                        let filter = background
                            .state
                            .lock()
                            .expect("navigation view poisoned")
                            .view
                            .filter
                            .clone();
                        let result = background
                            .execute(NavigationCommand::Query { filter })
                            .await;
                        background
                            .state
                            .lock()
                            .expect("navigation view poisoned")
                            .view
                            .diagnostic = result.err();
                        background
                            .work
                            .changed
                            .send_modify(|revision| *revision = revision.saturating_add(1));
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
