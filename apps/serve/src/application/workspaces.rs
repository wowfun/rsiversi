use super::Sessions;
use rsi_agent_session_protocol::SessionId;
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
    InsertBefore {
        id: WorkspaceId,
        before: Option<WorkspaceId>,
    },
    Membership,
    InsertSessionBefore {
        id: WorkspaceId,
        session: String,
        before: Option<String>,
    },
    SetPin {
        session: rsi_agent_session_protocol::SessionId,
        pinned: bool,
    },
    ArchiveActivity {
        session: SessionId,
    },
    Archive {
        session: SessionId,
        #[serde(default)]
        stop_activity: bool,
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
        let origin = match input.operation {
            Operation::ArchiveActivity { .. }
            | Operation::Archive { .. }
            | Operation::Unarchive { .. } => self.routes.credential(&input.request),
            _ => self.routes.caller(&input.request),
        }
        .map_err(|_| Error::Cancelled)?;
        let value = match input.operation {
            Operation::List { after } => {
                let registry = self.workspaces.scoped(origin);
                let order = if after.is_none() {
                    Some(registry.display_order().await.map_err(remote)?.ids)
                } else {
                    None
                };
                let page = registry.list(after, 16).await.map_err(remote)?;
                json!({"items":page.records.iter().map(record).collect::<Vec<_>>(),"next":page.next,"order":order})
            }
            Operation::InsertBefore { id, before } => {
                match self
                    .workspaces
                    .scoped(origin)
                    .insert_before(&id, before.as_ref())
                    .await
                {
                    Ok(order) => json!({"status":"applied","ids":order.ids}),
                    Err(rsi_workspace_protocol::WorkspaceError::Unknown(id)) => {
                        json!({"status":"unknown","id":id})
                    }
                    Err(error) => return Err(remote(error)),
                }
            }
            Operation::Membership => self.workspace_membership(&origin).await?,
            Operation::InsertSessionBefore {
                id,
                session,
                before,
            } => {
                self.move_workspace_session(origin, id, session, before)
                    .await?
            }
            Operation::SetPin { session, pinned } => {
                self.set_workspace_pin(origin, session, pinned).await?
            }
            Operation::ArchiveActivity { session } => {
                self.workspace_archive(origin, session, ArchiveIntent::Inspect)
                    .await?
            }
            Operation::Archive {
                session,
                stop_activity,
            } => {
                self.workspace_archive(
                    origin,
                    session,
                    if stop_activity {
                        ArchiveIntent::StopAndArchive
                    } else {
                        ArchiveIntent::Archive
                    },
                )
                .await?
            }
            Operation::Unarchive { session } => {
                self.workspace_archive(origin, session, ArchiveIntent::Restore)
                    .await?
            }
            Operation::Drafts { after } => self.workspace_drafts(&origin, after.as_ref()).await?,
            Operation::Default => self.default_workspace(origin).await?,
            Operation::Rename { id, title } => {
                let row = self
                    .workspaces
                    .scoped(origin.clone())
                    .rename(&id, title)
                    .await
                    .map_err(remote)?;
                self.workspace_account(origin, &row).await?
            }
            Operation::Create { path } => {
                let (row, created) = self
                    .workspaces
                    .register_local(origin.clone(), &path)
                    .await
                    .map_err(remote)?;
                let row = if created {
                    record(&row)
                } else {
                    self.workspace_account(origin, &row).await?
                };
                json!({"record":row,"created":created})
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

    async fn workspace_membership(&self, origin: &rsi_api_protocol::CallOrigin) -> Result<Value> {
        let seed = self
            .navigation
            .order_seed(origin, OrderScope::All)
            .map_err(remote)?
            .await
            .map_err(remote)?;
        let OrderMembership::Available { groups, members } = seed.membership else {
            return Err(Error::Capacity);
        };
        Ok(
            json!({"groups":groups.iter().map(|group| &group.workspace).collect::<Vec<_>>(),"members":members}),
        )
    }

    async fn workspace_account(
        &self,
        origin: rsi_api_protocol::CallOrigin,
        row: &WorkspaceRecord,
    ) -> Result<Value> {
        let rsi_api_protocol::CallOrigin::Device(device) = &origin else {
            return Err(Error::Protocol);
        };
        let drafts = self
            .ingress
            .device_drafts(device, Some(&row.id))
            .await
            .map_err(remote)?;
        let members = self
            .navigation
            .workspace_members(origin, row)
            .map_err(remote)?
            .await
            .map_err(remote)?;
        let mut drafts: std::collections::BTreeMap<_, _> = drafts
            .into_iter()
            .map(|draft| (draft.session_id, draft.created_at_ms))
            .collect();
        for (id, _) in &members {
            drafts.remove(id);
        }
        let mut updated = row.created_at_ms;
        let sessions = drafts
            .into_iter()
            .rev()
            .chain(members)
            .map(|(id, at)| {
                updated = updated.max(Some(at));
                id
            })
            .collect::<Vec<_>>();
        let mut value = record(row);
        value["sessions"] = json!(sessions);
        value["updated"] = json!(updated);
        Ok(value)
    }

    async fn move_workspace_session(
        &self,
        origin: rsi_api_protocol::CallOrigin,
        id: WorkspaceId,
        session: String,
        before: Option<String>,
    ) -> Result<Value> {
        let parsed = SessionId::new(session).and_then(|session| {
            before
                .map(SessionId::new)
                .transpose()
                .map(|before| (session, before))
        });
        if let Ok((session, before)) = parsed {
            use rsi_navigation::WorkspaceMoveOutcome;
            let outcome = self
                .navigation
                .insert_workspace_session_before(origin.clone(), id, session, before)
                .map_err(remote)?
                .await
                .map_err(remote)?;
            Ok(match outcome {
                WorkspaceMoveOutcome::Applied(row) => {
                    json!({"status":"applied","record":self.workspace_account(origin, &row).await?})
                }
                WorkspaceMoveOutcome::UnknownWorkspace => json!({"status":"unknown"}),
                WorkspaceMoveOutcome::InvalidMember => json!({"status":"invalid_member"}),
            })
        } else {
            Ok(json!({"status":"invalid_member"}))
        }
    }

    async fn default_workspace(&self, origin: rsi_api_protocol::CallOrigin) -> Result<Value> {
        let spec = rsi_workspace_default_api::operation();
        let invocation = self
            .dispatch
            .admit(&spec.call(), origin.clone())
            .map_err(remote)?;
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
        match selected {
            Some(row) => self.workspace_account(origin, &row).await,
            None => Ok(Value::Null),
        }
    }

    async fn workspace_drafts(
        &self,
        origin: &rsi_api_protocol::CallOrigin,
        after: Option<&SessionId>,
    ) -> Result<Value> {
        let rsi_api_protocol::CallOrigin::Device(device) = origin else {
            return Err(Error::Protocol);
        };
        let rows = self
            .ingress
            .device_drafts(device, None)
            .await
            .map_err(remote)?;
        let mut slice = rows
            .iter()
            .filter(|row| after.is_none_or(|id| id < &row.session_id));
        let mut items = Vec::new();
        for row in slice.by_ref().take(8) {
            items.push(json!({
                "id": row.session_id, "cwd": row.coordinates.path(),
                "workspace": row.workspace_id,
                "createdAt": row.created_at_ms, "updatedAt": row.created_at_ms,
                "blank": row.inherited_turns == 0, "running": false,
            }));
        }
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
            ArchiveOutcome::Ready(activity) => Ok(json!({"status":"ready","activity":activity})),
            ArchiveOutcome::Applied { changed, ids } => {
                Ok(json!({"status":"applied","changed":changed,"archivedSessionIds":ids}))
            }
        }
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

fn record(row: &WorkspaceRecord) -> Value {
    json!({"id":row.id,"path":row.coordinates.path(),"title":row.name,"created":row.created_at_ms})
}
fn remote(error: impl std::fmt::Display) -> Error {
    Error::Remote(error.to_string().chars().take(1000).collect())
}
