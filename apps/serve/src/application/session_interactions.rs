use super::{
    Sessions,
    session_history::{fragment_text, resource_key},
};
use futures_util::StreamExt;
use rsi_agent_session_protocol::SessionId;
use rsi_approval_protocol::ApprovalDecision;
use rsi_extension::{Error, Result};
use rsi_session_protocol::InteractionStream;
use rsi_user_questions_protocol::QuestionAnswer;
use serde::Deserialize;
use serde_json::{Value, json};
use std::{fmt, sync::Arc};
use tokio::sync::Mutex;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Input {
    request: String,
    session_id: SessionId,
    operation: Operation,
}
#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
enum Operation {
    Open {
        key: String,
    },
    Next {
        key: String,
    },
    Close {
        key: String,
    },
    Approval {
        owner: SessionId,
        id: String,
        decision: ApprovalDecision,
    },
    Question {
        id: String,
        answer: QuestionAnswer,
    },
}
struct Watch {
    session: SessionId,
    feed: Mutex<Feed>,
}
impl fmt::Debug for Watch {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("InteractionWatch")
            .field(&self.session)
            .finish()
    }
}
struct Feed {
    stream: InteractionStream,
    snapshot: Option<(String, usize)>,
}
impl Feed {
    async fn next(&mut self) -> Result<Value> {
        if self.snapshot.is_none() {
            let snapshot = self
                .stream
                .next()
                .await
                .ok_or(Error::Disconnected)?
                .map_err(remote)?;
            self.snapshot = Some((
                serde_json::to_string(&snapshot).map_err(|_| Error::Protocol)?,
                0,
            ));
        }
        let (json, offset) = self
            .snapshot
            .as_mut()
            .expect("retained interaction snapshot");
        let (end, part) = fragment_text(json, *offset)?;
        let value =
            json!({"json":part,"offset":*offset,"next_offset":end,"total_bytes":json.len()});
        *offset = end;
        if end == json.len() {
            self.snapshot = None;
        }
        Ok(value)
    }
}
fn remote(error: impl fmt::Display) -> Error {
    Error::Remote(error.to_string().chars().take(512).collect())
}
impl Sessions {
    pub(super) async fn interactions(&self, payload: Value) -> Result<Value> {
        let Input {
            request,
            session_id,
            operation,
        } = serde_json::from_value(payload).map_err(|_| Error::Protocol)?;
        match operation {
            Operation::Next { key } => {
                let (watch, retired) = self
                    .routes
                    .shared_resource::<Watch>(&request, &resource_key("interactions", &key)?)
                    .map_err(|_| Error::Cancelled)?;
                if watch.session != session_id {
                    return Err(Error::Protocol);
                }
                let mut feed = watch.feed.try_lock().map_err(|_| Error::Protocol)?;
                tokio::select! {biased; ()=retired.cancelled()=>Err(Error::Cancelled),value=feed.next()=>value}
            }
            Operation::Close { key } => {
                self.routes
                    .release_resource(&request, &resource_key("interactions", &key)?);
                Ok(Value::Null)
            }
            Operation::Open { key } => {
                let key = resource_key("interactions", &key)?;
                let handle = self.interactive_handle(&request, &session_id).await?;
                let stream = handle.observe_interactions().await.map_err(remote)?;
                let watch = Watch {
                    session: session_id,
                    feed: Mutex::new(Feed {
                        stream,
                        snapshot: None,
                    }),
                };
                self.routes
                    .retain_resource(&request, key, Arc::new(watch))
                    .map_err(|_| Error::Cancelled)?;
                Ok(Value::Null)
            }
            Operation::Approval {
                owner,
                id,
                decision,
            } => {
                let handle = self.interactive_handle(&request, &session_id).await?;
                Ok(
                    json!({"accepted":handle.answer_approval(&owner,&id,decision).await.map_err(remote)?}),
                )
            }
            Operation::Question { id, answer } => {
                let handle = self.interactive_handle(&request, &session_id).await?;
                Ok(json!({"accepted":handle.answer_question(&id,answer).await.map_err(remote)?}))
            }
        }
    }
}
