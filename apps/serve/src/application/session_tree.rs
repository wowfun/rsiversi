use super::Sessions;
use rsi_agent_session_protocol::{ExecutionOwner, SessionId};
use rsi_extension::{Error, Result};
use rsi_session_protocol::{ActivityStatus, SessionError};
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::BTreeMap;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TreeRequest {
    request: String,
    session_id: SessionId,
}

impl Sessions {
    pub(super) async fn descendants(&self, payload: Value) -> Result<Value> {
        let input: TreeRequest = serde_json::from_value(payload).map_err(|_| Error::Protocol)?;
        let service = self.ingress.scoped(
            self.routes
                .caller(&input.request)
                .map_err(|_| Error::Cancelled)?,
        );
        service
            .read_summary(&input.session_id)
            .await
            .map_err(|_| Error::Remote("Session unavailable to caller".into()))?;
        let mut tree = self
            .store
            .read_agent_subtree_snapshot(&input.session_id)
            .await
            .map_err(|_| Error::Remote("Agent subtree unavailable".into()))?;
        tree.descendants
            .sort_by(|left, right| left.path.cmp(&right.path));
        let mut depths = BTreeMap::from([(input.session_id, 0usize)]);
        let mut activity = None;
        let mut rows = Vec::new();
        for child in tree.descendants {
            // A hidden parent excludes its entire branch, including any path or label.
            let Some(depth) = depths.get(&child.parent_session_id).map(|depth| depth + 1) else {
                continue;
            };
            let id = child.status.session_id;
            let header = match service.read_summary(&id).await {
                Ok(summary) => summary.header,
                Err(
                    SessionError::Api(rsi_api_protocol::ApiError::Unauthorized)
                    | SessionError::NotFound(_),
                ) => continue,
                Err(_) => return Err(Error::Remote("Child Session header unavailable".into())),
            };
            let status = if child.status.has_open_turn {
                if activity.is_none() {
                    activity = Some(
                        service
                            .activity()
                            .await
                            .map_err(|_| Error::Remote("Agent activity unavailable".into()))?,
                    );
                }
                activity
                    .as_ref()
                    .and_then(|page| page.entries.iter().find(|item| item.session == id))
                    .map_or(ActivityStatus::Unknown, |item| item.status)
            } else {
                ActivityStatus::Idle
            };
            rows.push(json!({"id":id,"parent":child.parent_session_id,"depth":depth,"task":child.task_name,
                "activity":status,"created_at":header.created_at_ms(),"program_owned":matches!(child.execution_owner,ExecutionOwner::ProgramRun{..})}));
            depths.insert(id, depth);
        }
        self.routes
            .caller(&input.request)
            .map_err(|_| Error::Cancelled)?;
        Ok(Value::Array(rows))
    }
}
