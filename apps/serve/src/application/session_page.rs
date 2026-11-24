use super::Sessions;
use rsi_extension::{Error, Result};
use rsi_session_protocol::{RecentSessionCursor, SessionSummary};
use serde::Deserialize;
use serde_json::{Value, json};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PageRequest {
    request: String,
    after: Option<RecentSessionCursor>,
    #[serde(default)]
    history: bool,
}

impl Sessions {
    pub(super) async fn page(&self, payload: Value) -> Result<Value> {
        let request: PageRequest = serde_json::from_value(payload).map_err(|_| Error::Protocol)?;
        let origin = self
            .routes
            .caller(&request.request)
            .map_err(|_| Error::Cancelled)?;
        let service = self.ingress.scoped(origin);
        let page = service
            .list_recent(request.after.as_ref(), 64)
            .await
            .map_err(|_| Error::Remote("Session catalog unavailable".into()))?;
        let running = if request.history || page.sessions.is_empty() {
            std::collections::BTreeSet::new()
        } else {
            let ids: Vec<_> = page
                .sessions
                .iter()
                .map(|row| row.header.session_id().clone())
                .collect();
            self.projections
                .running_sessions(&ids)
                .map_err(|_| Error::Remote("Session activity unavailable".into()))?
        };
        let mut items = Vec::with_capacity(page.sessions.len());
        for row in &page.sessions {
            items.push(if request.history {
                super::session_history::stored_snapshot(
                    &row.header,
                    row.durable_fact_seq,
                    row.durable_control_seq,
                )
            } else {
                catalog_item(row, running.contains(row.header.session_id()))
            });
        }
        self.routes
            .caller(&request.request)
            .map_err(|_| Error::Cancelled)?;
        Ok(json!({"items":items,"next":page.next}))
    }
}

fn catalog_item(summary: &SessionSummary, running: bool) -> Value {
    let header = &summary.header;
    let mut item = json!({"id":header.session_id(),"cwd":header.canonical_cwd(),"createdAt":header.created_at_ms(),"updatedAt":summary.last_activity_ms,
        "blank":summary.is_blank(),"running":running});
    if let Some(origin) = header.fork_origin() {
        item["parent"] = json!(origin.parent.session_id());
    }
    item
}

/// One application generation owns the cursor across its baseline and subsequent reads.
pub(super) fn watch_directory(
    sessions: &Sessions,
    connection: &rsi_extension::Connection,
    owner: &str,
) -> rsi_meta::Result<rsi_extension::HandlerRegistration> {
    let rsi_api_protocol::CallOrigin::Device(device) =
        sessions.routes.caller(owner).map_err(super::failure)?
    else {
        return Err(super::failure("application directory requires its owner"));
    };
    connection
        .register_handler(
            "application.directory.wait",
            std::sync::Arc::new(Directory {
                owner: owner.into(),
                device,
                ingress: sessions.ingress.clone(),
                workspaces: sessions.workspaces.clone(),
                navigation: sessions.navigation.clone(),
                changes: tokio::sync::Mutex::default(),
            }),
        )
        .map_err(super::failure)
}

struct Directory {
    owner: String,
    device: rsi_api_protocol::AuthenticatedDevice,
    ingress: std::sync::Arc<dyn rsi_session_protocol::SessionIngress>,
    workspaces: std::sync::Arc<dyn rsi_workspace_protocol::WorkspaceIngress>,
    navigation: std::sync::Arc<rsi_navigation::Navigation>,
    changes: tokio::sync::Mutex<Option<DirectoryWatch>>,
}
struct DirectoryWatch {
    sessions: Option<rsi_session_protocol::DirectoryChanges>,
    workspaces: Option<tokio::sync::watch::Receiver<u64>>,
    navigation: tokio::sync::watch::Receiver<()>,
    fallback: Option<tokio::time::Interval>,
}

async fn when_present<F: Future>(future: Option<F>) -> F::Output {
    match future {
        Some(future) => future.await,
        None => std::future::pending().await,
    }
}

impl DirectoryWatch {
    fn new(
        sessions: Option<rsi_session_protocol::DirectoryChanges>,
        workspaces: Option<tokio::sync::watch::Receiver<u64>>,
        navigation: tokio::sync::watch::Receiver<()>,
    ) -> Self {
        let fallback = (sessions.is_none() || workspaces.is_none()).then(|| {
            let period = std::time::Duration::from_secs(1);
            let mut interval =
                tokio::time::interval_at(tokio::time::Instant::now() + period, period);
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            interval
        });
        Self {
            sessions,
            workspaces,
            navigation,
            fallback,
        }
    }

    async fn next(&mut self) -> Result<Value> {
        use futures_util::StreamExt;
        let refresh_sessions = self.sessions.is_none();
        tokio::select! {
            row = when_present(self.sessions.as_mut().map(|stream| stream.next())) => match row {
                Some(Ok(change)) => Ok(match change {
                    rsi_session_protocol::DirectoryChange::Catalog => json!("all"),
                    rsi_session_protocol::DirectoryChange::SharedBlank(id) => json!({"blank":id}),
                    rsi_session_protocol::DirectoryChange::RecheckSharedBlanks => json!({"blank":null}),
                }),
                Some(Err(error)) => Err(Error::Remote(error.to_string())),
                None => Err(Error::Cancelled),
            },
            result = when_present(self.workspaces.as_mut().map(|changes| changes.changed())) => {
                result.map_err(|_| Error::Cancelled)?;
                self.navigation.mark_unchanged();
                Ok(json!("workspace"))
            },
            result = self.navigation.changed() => {
                result.map_err(|_| Error::Cancelled)?;
                if let Some(changes) = &mut self.workspaces {
                    changes.mark_unchanged();
                }
                Ok(json!("workspace"))
            },
            _ = when_present(self.fallback.as_mut().map(|interval| interval.tick())) => {
                Ok(if refresh_sessions { json!("all") } else { json!("workspace") })
            },
        }
    }
}
impl std::fmt::Debug for Directory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Directory").finish_non_exhaustive()
    }
}

#[async_trait::async_trait]
impl rsi_extension::Handler for Directory {
    async fn call(
        &self,
        _method: &str,
        payload: Value,
        stop: tokio_util::sync::CancellationToken,
    ) -> Result<Value> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Request {
            request: String,
            open: bool,
        }
        let request: Request = serde_json::from_value(payload).map_err(|_| Error::Protocol)?;
        if request.request != self.owner || self.device.revoked.is_cancelled() {
            return Err(Error::Cancelled);
        }
        let mut changes = self.changes.try_lock().map_err(|_| Error::Capacity)?;
        if request.open {
            changes.take();
            let sessions = self
                .ingress
                .watch_directory(&self.device)
                .map_err(|e| Error::Remote(e.to_string()))?;
            *changes = Some(DirectoryWatch::new(
                sessions,
                self.workspaces.directory_changes(),
                self.navigation.metadata_changes(),
            ));
            return Ok(Value::Null);
        }
        let watch = changes.as_mut().ok_or(Error::Protocol)?;
        let result = tokio::select! {
            biased;
            () = stop.cancelled() => Err(Error::Cancelled),
            () = self.device.revoked.cancelled() => Err(Error::Cancelled),
            result = watch.next() => result,
        };
        if result.is_err() {
            changes.take();
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rsi_agent_session_protocol::{
        AgentPresetId, FrozenAgentSettings, SessionHeader, SessionId,
    };

    #[tokio::test]
    async fn directory_coalesces_commits_and_preserves_the_session_observation() {
        use futures_util::FutureExt;
        let (workspaces, registrations) = tokio::sync::watch::channel(0);
        let (navigation, metadata) = tokio::sync::watch::channel(());
        let mut watch = DirectoryWatch::new(
            Some(Box::pin(futures_util::stream::pending())),
            Some(registrations),
            metadata,
        );
        assert!(watch.fallback.is_none());
        assert!(watch.next().now_or_never().is_none());
        workspaces.send_replace(1);
        navigation.send_replace(());
        assert_eq!(watch.next().now_or_never().unwrap().unwrap(), "workspace");
        assert!(watch.next().now_or_never().is_none());
        navigation.send_replace(());
        assert_eq!(watch.next().now_or_never().unwrap().unwrap(), "workspace");
        watch.sessions = Some(Box::pin(futures_util::stream::iter([Ok(
            rsi_session_protocol::DirectoryChange::RecheckSharedBlanks,
        )])));
        assert_eq!(watch.next().await.unwrap(), json!({"blank":null}));
        assert!(matches!(watch.next().await, Err(Error::Cancelled)));
    }

    #[tokio::test]
    async fn directory_reconciles_providers_without_observations() {
        let (navigation, _) = tokio::sync::watch::channel(());
        for sessions_missing in [false, true] {
            let sessions =
                (!sessions_missing).then(|| Box::pin(futures_util::stream::pending()) as _);
            let mut watch = DirectoryWatch::new(sessions, None, navigation.subscribe());
            watch.fallback.as_mut().unwrap().reset_immediately();
            let expected = if sessions_missing { "all" } else { "workspace" };
            assert_eq!(watch.next().await.unwrap(), expected);
        }
    }

    #[test]
    fn catalog_keeps_accepted_context_visible_before_the_first_turn() {
        let header = SessionHeader::new_local(
            SessionId::new("held-context").unwrap(),
            1,
            "/workspace",
            AgentPresetId::new("test").unwrap(),
            FrozenAgentSettings::new(
                "default",
                "system",
                rsi_ai_protocol::ModelRef::new("test", "test").unwrap(),
                rsi_sandbox::SandboxMode::WorkspaceWrite,
                false,
            )
            .unwrap(),
        )
        .unwrap();
        let mut summary = SessionSummary {
            working_directory: None,
            header,
            last_activity_ms: 2,
            durable_fact_seq: 0,
            durable_control_seq: 2,
        };
        assert_eq!(
            catalog_item(&summary, false),
            json!({
                "id":"held-context","cwd":"/workspace","createdAt":1,"updatedAt":2,"blank":false,"running":false,
            })
        );
        summary.durable_control_seq = 0;
        assert_eq!(catalog_item(&summary, false)["blank"], true);
        summary.durable_fact_seq = 1;
        let started = catalog_item(&summary, true);
        assert_eq!(started["blank"], false);
        assert_eq!(started["running"], true);
    }
}
