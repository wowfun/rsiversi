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
        "blank":summary.durable_fact_seq==0 && summary.durable_control_seq==0,"running":running});
    if let Some(origin) = header.fork_origin() {
        item["parent"] = json!(origin.history.parent_session_id);
    }
    item
}

#[cfg(test)]
mod tests {
    use super::*;
    use rsi_agent_session_protocol::{
        AgentPresetId, FrozenAgentSettings, SessionHeader, SessionId,
    };

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
    workspaces: Option<u64>,
    navigation: u64,
    tick: tokio::time::Interval,
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
        use futures_util::StreamExt;
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
            let interval = std::time::Duration::from_secs(1);
            let mut tick =
                tokio::time::interval_at(tokio::time::Instant::now() + interval, interval);
            tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            *changes = Some(DirectoryWatch {
                sessions,
                tick,
                workspaces: self.workspaces.directory_revision(),
                navigation: self.navigation.metadata_revision(),
            });
            return Ok(Value::Null);
        }
        let watch = changes.as_mut().ok_or(Error::Protocol)?;
        let result = loop {
            let next = async {
                match &mut watch.sessions {
                    Some(stream) => stream.next().await,
                    None => std::future::pending().await,
                }
            };
            tokio::select! {
                biased;
                () = stop.cancelled() => break Err(Error::Cancelled),
                () = self.device.revoked.cancelled() => break Err(Error::Cancelled),
                row = next => break match row {
                    Some(Ok(())) => Ok(json!("all")),
                    Some(Err(error)) => Err(Error::Remote(error.to_string())),
                    None => Err(Error::Cancelled),
                },
                _ = watch.tick.tick() => {
                    let workspaces = self.workspaces.directory_revision();
                    let navigation = self.navigation.metadata_revision();
                    if watch.sessions.is_none() { break Ok(json!("all")); }
                    if workspaces.is_none() || watch.workspaces != workspaces || watch.navigation != navigation {
                        watch.workspaces = workspaces;
                        watch.navigation = navigation;
                        break Ok(json!("workspace"));
                    }
                }
            }
        };
        if result.is_err() {
            changes.take();
        }
        result
    }
}
