use super::{
    Sessions,
    session_history::{fragment_text, resource_key},
};
use futures_util::StreamExt;
use rsi_agent_session_protocol::{EffectId, ModelEventCursor, ModelEventPurpose, SessionId};
use rsi_agent_turn_protocol::{ModelLiveStatus, ObservedModel};
use rsi_extension::{Error, Result};
use rsi_session_protocol::{ModelLiveStream, SessionHandle};
use serde_json::{Value, json};
use std::{fmt, sync::Arc};
use tokio::sync::Mutex;

struct Reader {
    session: SessionId,
    state: Mutex<Feed>,
}
impl fmt::Debug for Reader {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ModelReader")
            .field("session", &self.session)
            .finish_non_exhaustive()
    }
}
struct Feed {
    effect: EffectId,
    updates: ModelLiveStream,
    current: Option<ObservedModel>,
    cursor: ModelEventCursor,
    batch: Option<Batch>,
}
struct Batch {
    json: String,
    offset: usize,
    revision: u64,
    status: ModelLiveStatus,
    complete: bool,
}
impl Feed {
    async fn next(&mut self) -> Result<Value> {
        if self.batch.is_none() {
            while self.current.is_none() {
                let snapshot = self
                    .updates
                    .next()
                    .await
                    .ok_or(Error::Disconnected)?
                    .map_err(|e| Error::Remote(e.to_string()))?;
                if snapshot.as_ref().effect_id == self.effect
                    && snapshot.as_ref().purpose == ModelEventPurpose::Conversation
                {
                    self.current = Some(snapshot);
                }
            }
            let current = self
                .current
                .as_ref()
                .expect("selected model prefix")
                .as_ref();
            let mut encoded = String::from("[");
            for item in current.prefix.events_after(&mut self.cursor).take(64) {
                let event = item.map_err(|e| Error::Remote(e.to_string()))?;
                if encoded.len() > 1 {
                    encoded.push(',');
                }
                encoded.push_str(&serde_json::to_string(&event).map_err(|_| Error::Protocol)?);
                if encoded.len() >= 64 * 1024 {
                    break;
                }
            }
            encoded.push(']');
            let complete = self.cursor.event_count() >= current.prefix.event_count();
            self.batch = Some(Batch {
                json: encoded,
                offset: 0,
                revision: current.revision,
                status: current.status.clone(),
                complete,
            });
            if complete {
                self.current = None;
            }
        }
        let batch = self.batch.as_mut().expect("prepared model page");
        let (end, part) = fragment_text(&batch.json, batch.offset)?;
        let value = json!({"json":part,"offset":batch.offset,"next_offset":end,"total_bytes":batch.json.len(),
            "revision":batch.revision,"status":batch.status,"complete":batch.complete});
        batch.offset = end;
        if end == batch.json.len() {
            self.batch = None;
        }
        Ok(value)
    }
}
impl Sessions {
    pub(super) async fn open_model(
        &self,
        request: &str,
        id: &str,
        session: SessionId,
        effect: EffectId,
        handle: &dyn SessionHandle,
    ) -> Result<Value> {
        let key = resource_key("model", id)?;
        let updates = handle
            .observe_models()
            .await
            .map_err(|e| Error::Remote(e.to_string()))?;
        let reader = Arc::new(Reader {
            session,
            state: Mutex::new(Feed {
                effect,
                updates,
                current: None,
                cursor: ModelEventCursor::default(),
                batch: None,
            }),
        });
        self.routes
            .retain_resource(request, key, reader)
            .map_err(|e| Error::Remote(e.to_string()))?;
        Ok(Value::Null)
    }
    pub(super) async fn next_model(
        &self,
        request: &str,
        id: &str,
        session: &SessionId,
    ) -> Result<Value> {
        let (reader, retired) = self
            .routes
            .shared_resource::<Reader>(request, &resource_key("model", id)?)
            .map_err(|_| Error::Cancelled)?;
        if &reader.session != session {
            return Err(Error::Protocol);
        }
        let mut feed = reader.state.try_lock().map_err(|_| Error::Protocol)?;
        tokio::select! {biased;()=retired.cancelled()=>Err(Error::Cancelled),value=feed.next()=>value}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rsi_agent_session_protocol::{ModelAttemptBuilder, TurnId};
    use rsi_agent_turn_protocol::{ModelLiveSnapshot, ObservationRetention};

    #[tokio::test]
    async fn a_slow_reader_fragments_one_event_and_continues_from_its_native_cursor() {
        let effect = EffectId::new("attempt").unwrap();
        let mut builder = ModelAttemptBuilder::new();
        let start = serde_json::from_value(
            json!({"type":"content_started","index":0,"content":{"type":"text"}}),
        )
        .unwrap();
        let text = "界".repeat(50_000);
        let delta = serde_json::from_value(
            json!({"type":"content_delta","index":0,"delta":{"type":"text","value":text}}),
        )
        .unwrap();
        builder.push(0, &start).unwrap();
        builder.push(1, &delta).unwrap();
        let snapshot = |builder: &ModelAttemptBuilder, revision| ModelLiveSnapshot {
            session_id: SessionId::new("session").unwrap(),
            turn_id: TurnId::new("turn").unwrap(),
            effect_id: effect.clone(),
            purpose: ModelEventPurpose::Conversation,
            revision,
            prefix: builder.prefix(),
            status: ModelLiveStatus::Streaming,
        };
        let pool = ObservationRetention::default();
        let first = pool.retain_model(Arc::new(snapshot(&builder, 1))).unwrap();
        builder
            .push(
                2,
                &serde_json::from_value(json!({"type":"content_finished","index":0})).unwrap(),
            )
            .unwrap();
        let next = pool.retain_model(Arc::new(snapshot(&builder, 2))).unwrap();
        let mut feed = Feed {
            effect,
            updates: Box::pin(futures_util::stream::iter([Ok(first), Ok(next)])),
            current: None,
            cursor: ModelEventCursor::default(),
            batch: None,
        };
        let a = feed.next().await.unwrap();
        let b = feed.next().await.unwrap();
        assert_eq!(a["next_offset"], b["offset"]);
        assert_eq!(a["revision"], 1);
        let encoded = format!(
            "{}{}",
            a["json"].as_str().unwrap(),
            b["json"].as_str().unwrap()
        );
        let items: Vec<Value> = serde_json::from_str(&encoded).unwrap();
        assert_eq!(items.len(), 2);
        assert_eq!(items[1][1]["delta"]["value"], text);
        let c = feed.next().await.unwrap();
        assert_eq!(c["revision"], 2);
        let items: Vec<Value> = serde_json::from_str(c["json"].as_str().unwrap()).unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0][1]["type"], "content_finished");
    }
}
