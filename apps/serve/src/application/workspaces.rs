use super::Sessions;
use rsi_agent_session_protocol::SessionId;
use rsi_agent_store_protocol::{
    StoreAgentDescendantStatus, StoreAgentSessionStatus, StoreAgentSubtreeSnapshot,
};
use rsi_extension::{Error, Result};
use rsi_navigation::{ArchiveIntent, ArchiveOutcome};
use rsi_navigation_api::{OrderMembership, OrderScope};
use rsi_workspace_protocol::{WorkspaceCursor, WorkspaceId, WorkspaceName, WorkspaceRecord};
use serde::Deserialize;
use serde_json::{Value, json};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Input {
    request: String,
    operation: Operation,
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
enum Operation {
    List {
        after: Option<WorkspaceCursor>,
    },
    Membership,
    SetPin {
        session: rsi_agent_session_protocol::SessionId,
        pinned: bool,
    },
    ArchiveActivity {
        session: SessionId,
    },
    Archive {
        session: SessionId,
    },
    StopArchiveActivity {
        session: SessionId,
        stop_root: bool,
    },
    Unarchive {
        session: SessionId,
    },
    Drafts {
        after: Option<rsi_agent_session_protocol::SessionId>,
    },
    Default,
    Create {
        path: std::path::PathBuf,
    },
    Rename {
        id: WorkspaceId,
        title: WorkspaceName,
    },
    Delete {
        id: WorkspaceId,
    },
}

impl Sessions {
    pub(super) async fn workspace_operation(&self, payload: Value) -> Result<Value> {
        let input: Input = serde_json::from_value(payload).map_err(|_| Error::Protocol)?;
        let origin = self
            .routes
            .caller(&input.request)
            .map_err(|_| Error::Cancelled)?;
        let value = match input.operation {
            Operation::List { after } => {
                let page = self
                    .workspaces
                    .scoped(origin)
                    .list(after, 16)
                    .await
                    .map_err(remote)?;
                json!({"items":page.records.iter().map(record).collect::<Vec<_>>(),"next":page.next})
            }
            Operation::Membership => {
                let seed = self
                    .navigation
                    .order_seed(&origin, OrderScope::All)
                    .map_err(remote)?
                    .await
                    .map_err(remote)?;
                let OrderMembership::Available { groups, members } = seed.membership else {
                    return Err(Error::Capacity);
                };
                json!({"groups":groups.iter().map(WorkspaceId::from_coordinates).collect::<Vec<_>>(),"members":members})
            }
            Operation::SetPin { session, pinned } => {
                self.set_workspace_pin(origin, session, pinned).await?
            }
            Operation::ArchiveActivity { session } => {
                self.workspace_archive(origin, session, ArchiveIntent::Inspect)
                    .await?
            }
            Operation::StopArchiveActivity { session, stop_root } => {
                self.stop_archive_activity(origin, &session, stop_root)
                    .await?;
                Value::Null
            }
            Operation::Archive { session } => {
                self.workspace_archive(origin, session, ArchiveIntent::Archive)
                    .await?
            }
            Operation::Unarchive { session } => {
                self.workspace_archive(origin, session, ArchiveIntent::Restore)
                    .await?
            }
            Operation::Drafts { after } => self.workspace_drafts(&origin, after.as_ref()).await?,
            Operation::Default => {
                let spec = rsi_workspace_default_api::operation();
                let invocation = self.dispatch.admit(&spec.call(), origin).map_err(remote)?;
                let bytes = self
                    .dispatch
                    .input_budget(spec.class)
                    .copy(b"{}")
                    .map_err(remote)?;
                let rsi_api_protocol::ApiOutput::Reply(message) =
                    invocation.invoke(bytes).await.map_err(remote)?
                else {
                    return Err(Error::Protocol);
                };
                let selected: Option<WorkspaceRecord> =
                    serde_json::from_slice(message.json.as_bytes()).map_err(|_| Error::Protocol)?;
                selected.as_ref().map_or(Value::Null, record)
            }
            Operation::Rename { id, title } => record(
                &self
                    .workspaces
                    .scoped(origin)
                    .rename(&id, title)
                    .await
                    .map_err(remote)?,
            ),
            Operation::Create { path } => {
                let (row, created) = self
                    .workspaces
                    .register_local(origin, &path)
                    .await
                    .map_err(remote)?;
                json!({"record":record(&row),"created":created})
            }
            Operation::Delete { id } => {
                json!(
                    self.workspaces
                        .scoped(origin)
                        .delete_registration(&id)
                        .await
                        .map_err(remote)?
                )
            }
        };
        self.routes
            .caller(&input.request)
            .map_err(|_| Error::Cancelled)?;
        Ok(value)
    }

    async fn workspace_drafts(
        &self,
        origin: &rsi_api_protocol::CallOrigin,
        after: Option<&SessionId>,
    ) -> Result<Value> {
        let rsi_api_protocol::CallOrigin::Device(device) = origin else {
            return Err(Error::Protocol);
        };
        let rows = self.ingress.device_drafts(device).await.map_err(remote)?;
        let mut slice = rows
            .iter()
            .filter(|row| after.is_none_or(|id| id < &row.session_id));
        let items: Vec<_> = slice
            .by_ref()
            .take(8)
            .map(|row| {
                json!({
                    "id": row.session_id, "cwd": row.coordinates.path(),
                    "workspace": WorkspaceId::from_coordinates(&row.coordinates),
                    "createdAt": row.created_at_ms, "updatedAt": row.created_at_ms,
                    "blank": row.inherited_turns == 0, "running": false,
                })
            })
            .collect();
        let next = slice
            .next()
            .and_then(|_| items.last())
            .map(|row| &row["id"]);
        Ok(json!({"items":items,"next":next}))
    }

    async fn workspace_archive(
        &self,
        origin: rsi_api_protocol::CallOrigin,
        session: SessionId,
        intent: ArchiveIntent,
    ) -> Result<Value> {
        let result = self
            .navigation
            .archive_intent(origin, session.clone(), intent)
            .map_err(remote)?
            .await
            .map_err(remote)?;
        match result {
            ArchiveOutcome::Unknown => Ok(json!({"status":"unknown"})),
            ArchiveOutcome::Ready => {
                let tree = self
                    .store
                    .read_agent_subtree_snapshot(&session)
                    .await
                    .map_err(remote)?;
                let subagents = active_subagents(&tree)
                    .map(|child| json!({"id":child.status.session_id,"label":child.task_name}))
                    .collect::<Vec<_>>();
                let mut activity = Vec::new();
                if !subagents.is_empty() {
                    activity.push(json!({"kind":"subagent","items":subagents}));
                }
                if tree.session.has_open_turn {
                    let jobs = self.jobs.active_jobs(&session).map_err(remote)?;
                    if !jobs.is_empty() {
                        activity.push(json!({"kind":"job","items":jobs}));
                    }
                }
                if running(&tree.session) {
                    activity.push(json!({"kind":"turn"}));
                }
                Ok(json!({"status":"ready","activity":activity}))
            }
            ArchiveOutcome::Applied { changed, ids } => {
                Ok(json!({"status":"applied","changed":changed,"archivedSessionIds":ids}))
            }
        }
    }

    async fn stop_archive_activity(
        &self,
        origin: rsi_api_protocol::CallOrigin,
        session: &SessionId,
        stop_root: bool,
    ) -> Result<()> {
        const STOP: rsi_agent_turn_protocol::CancelTarget =
            rsi_agent_turn_protocol::CancelTarget::Session { keep_inbox: false };
        let service = self.ingress.scoped(origin);
        let root = service.attach(session).await.map_err(remote)?;
        let tree = self
            .store
            .read_agent_subtree_snapshot(session)
            .await
            .map_err(remote)?;
        let mut result = Ok(());
        if stop_root && running(&tree.session) {
            result = root.cancel(STOP, None).await.map(|_| ()).map_err(remote);
        }
        for child in active_subagents(&tree) {
            let stop = async {
                let handle = service.attach(&child.status.session_id).await?;
                handle.cancel(STOP, None).await
            }
            .await;
            if let Err(error) = stop {
                result = result.and(Err(remote(error)));
            }
        }
        result
    }

    async fn set_workspace_pin(
        &self,
        origin: rsi_api_protocol::CallOrigin,
        session: rsi_agent_session_protocol::SessionId,
        pinned: bool,
    ) -> Result<Value> {
        use rsi_navigation::PinOutcome;
        use rsi_navigation_api::PinnedEntry;
        let reply = match self
            .navigation
            .set_pin(origin.clone(), session, pinned)
            .map_err(remote)?
            .await
            .map_err(remote)?
        {
            PinOutcome::Unknown => json!({"status":"unknown"}),
            PinOutcome::Archived => json!({"status":"archived"}),
            PinOutcome::Applied => {
                let pins = self
                    .navigation
                    .pinned(origin, rsi_navigation_api::NavigationFilter::default())
                    .map_err(remote)?
                    .await
                    .map_err(remote)?;
                let ids = pins
                    .entries
                    .into_iter()
                    .map(|entry| match entry {
                        PinnedEntry::Available { entry } => entry.session,
                        PinnedEntry::Missing { session, .. } => session,
                    })
                    .collect::<Vec<_>>();
                json!({"status":"applied","pinnedSessionIds":ids})
            }
        };
        Ok(reply)
    }
}

fn running(state: &StoreAgentSessionStatus) -> bool {
    state.has_open_turn || state.has_active_activation || state.has_waking_message
}

fn active_subagents(
    tree: &StoreAgentSubtreeSnapshot,
) -> impl Iterator<Item = &StoreAgentDescendantStatus> {
    tree.descendants
        .iter()
        .filter(|child| running(&child.status) && tree.activation_owns(child))
}

fn record(row: &WorkspaceRecord) -> Value {
    json!({"id":row.id,"path":row.coordinates.path(),"title":row.name,"created":row.created_at_ms})
}
fn remote(error: impl std::fmt::Display) -> Error {
    Error::Remote(error.to_string().chars().take(1000).collect())
}
