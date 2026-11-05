use super::Sessions;
use futures_util::StreamExt;
use rsi_agent_session_protocol::SessionId;
use rsi_agent_turn_protocol::{ObservationCursor, SessionObservation};
use rsi_extension::{Error, Result};
use rsi_session_protocol::SessionHandle;
use serde::Deserialize;
use serde_json::{Value, json};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct HistoryRead {
    request: String,
    session_id: SessionId,
    parent_session_id: Option<SessionId>,
    operation: Operation,
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
enum Operation {
    SeedOpen {
        key: String,
    },
    SeedRead(super::session_inherited::ReadInterval),
    SeedClose {
        key: String,
    },
    Open {
        #[serde(default)]
        metadata_only: bool,
    },
    Controls {
        after_seq: u64,
        through_seq: u64,
    },
    Message {
        message_id: rsi_agent_session_protocol::MessageId,
        accepted_control_seq: u64,
        offset: usize,
    },
    Fact {
        after_seq: u64,
        offset: usize,
    },
    Attempt {
        after_seq: u64,
        offset: usize,
    },
    Wait {
        cursor: ObservationCursor,
    },
    ModelOpen {
        key: String,
        effect: rsi_agent_session_protocol::EffectId,
    },
    ModelNext {
        key: String,
    },
    ModelClose {
        key: String,
    },
}

impl Sessions {
    pub(super) async fn read_handle(
        &self,
        request: &str,
        session: &SessionId,
        parent: Option<&SessionId>,
    ) -> Result<std::sync::Arc<dyn SessionHandle>> {
        self.find_handle(request, session, parent)
            .await?
            .ok_or_else(|| Error::Remote("Session unavailable to caller".into()))
    }

    async fn find_handle(
        &self,
        request: &str,
        session: &SessionId,
        parent: Option<&SessionId>,
    ) -> Result<Option<std::sync::Arc<dyn SessionHandle>>> {
        let origin = self.routes.caller(request).map_err(|_| Error::Cancelled)?;
        let service = self.ingress.scoped(origin);
        let handle = match service.attach(session).await {
            Ok(handle) => handle,
            Err(
                rsi_session_protocol::SessionError::NotFound(_)
                | rsi_session_protocol::SessionError::Api(rsi_api_protocol::ApiError::Unauthorized),
            ) => return Ok(None),
            Err(_) => return Err(Error::Remote("Session unavailable to caller".into())),
        };
        if let Some(parent) = parent {
            service
                .read_header(parent)
                .await
                .map_err(|_| Error::Remote("Parent unavailable to caller".into()))?;
            let header = handle.header().await.map_err(|error| remote(&error))?;
            if header
                .fork_origin()
                .map(|fork| &fork.history.parent_session_id)
                != Some(parent)
            {
                return Err(Error::Remote(
                    "Session address does not identify a direct child".into(),
                ));
            }
        }
        Ok(Some(handle))
    }

    pub(super) async fn history(&self, payload: Value) -> Result<Value> {
        let input: HistoryRead = serde_json::from_value(payload).map_err(|_| Error::Protocol)?;
        let Some(handle) = self
            .find_handle(
                &input.request,
                &input.session_id,
                input.parent_session_id.as_ref(),
            )
            .await?
        else {
            return match input.operation {
                Operation::Open { .. } => Ok(Value::Null),
                _ => Err(Error::Remote("Session unavailable to caller".into())),
            };
        };
        let result = match input.operation {
            Operation::SeedOpen { key } => {
                self.open_inherited(
                    &input.request,
                    &key,
                    &handle.header().await.map_err(|error| remote(&error))?,
                )
                .await?
            }
            Operation::SeedRead(read) => {
                self.read_inherited(&input.request, &input.session_id, read)
                    .await?
            }
            Operation::SeedClose { key } => {
                self.routes
                    .release_resource(&input.request, &resource_key("seed", &key)?);
                Value::Null
            }
            Operation::Open { metadata_only } => {
                self.opening_snapshot(handle.as_ref(), metadata_only)
                    .await?
            }
            Operation::Controls {
                after_seq,
                through_seq,
            } => {
                self.history_controls(
                    &handle.header().await.map_err(|error| remote(&error))?,
                    after_seq,
                    through_seq,
                )
                .await?
            }
            Operation::Message {
                message_id,
                accepted_control_seq,
                offset,
            } => {
                let message = handle
                    .read_message(&message_id, accepted_control_seq)
                    .await
                    .map_err(|error| remote(&error))?;
                let encoded = serde_json::to_string(&message).map_err(|_| Error::Protocol)?;
                fragment(&encoded, offset, accepted_control_seq)?
            }
            Operation::Fact { after_seq, offset } => {
                read(handle.as_ref(), after_seq, offset, false).await?
            }
            Operation::Attempt { after_seq, offset } => {
                read(handle.as_ref(), after_seq, offset, true).await?
            }
            Operation::Wait { cursor } => {
                wait(handle.as_ref(), cursor).await?;
                let current = self
                    .store
                    .read_watermarks(&input.session_id)
                    .await
                    .map_err(|error| Error::Remote(error.to_string()))?;
                json!({"control_seq":current.durable_control_seq,"fact_seq":current.durable_fact_seq})
            }
            Operation::ModelOpen { key, effect } => {
                self.open_model(
                    &input.request,
                    &key,
                    input.session_id,
                    effect,
                    handle.as_ref(),
                )
                .await?
            }
            Operation::ModelNext { key } => {
                self.next_model(&input.request, &key, &input.session_id)
                    .await?
            }
            Operation::ModelClose { key } => {
                self.routes
                    .release_resource(&input.request, &resource_key("model", &key)?);
                Value::Null
            }
        };
        self.routes
            .caller(&input.request)
            .map_err(|_| Error::Cancelled)?;
        Ok(result)
    }

    async fn opening_snapshot(
        &self,
        handle: &dyn SessionHandle,
        metadata_only: bool,
    ) -> Result<Value> {
        let header = handle.header().await.map_err(|error| remote(&error))?;
        if metadata_only {
            let rows = self
                .store
                .session_activity_summaries(std::slice::from_ref(header.session_id()))
                .await
                .map_err(|_| Error::Remote("Session metadata unavailable".into()))?;
            return match rows.as_slice() {
                [Some(row)]
                    if row.session_id == *header.session_id()
                        && row.created_at_ms == header.created_at_ms() =>
                {
                    Ok(stored_snapshot(&header, row))
                }
                [None] => Ok(Value::Null),
                _ => Err(Error::Remote("Session catalog changed during read".into())),
            };
        }
        let (selection, cursor) = current_selection(handle, &header).await?;
        let through = match cursor {
            rsi_agent_session_protocol::ProjectionCursor::Durable { fact_seq, .. } => fact_seq,
            rsi_agent_session_protocol::ProjectionCursor::Draft { .. } => 0,
        };
        let mut metadata = stored_header(&header);
        metadata["through"] = json!(through);
        metadata["model_selection"] = json!(selection);
        metadata["selection_cursor"] = json!(cursor);
        metadata["model_baseline"] = json!({"model":header.settings().default_model(),
        "reasoning_effort":header.settings().default_reasoning_effort()});
        Ok(metadata)
    }
}

pub(super) fn stored_snapshot(
    header: &rsi_agent_session_protocol::SessionHeader,
    row: &rsi_agent_store_protocol::StoreActivityRow,
) -> Value {
    let mut item = stored_header(header);
    item["selection_cursor"] = json!(rsi_agent_session_protocol::ProjectionCursor::Durable {
        fact_seq: row.durable_fact_seq,
        control_seq: row.durable_control_seq,
    });
    item
}

fn stored_header(header: &rsi_agent_session_protocol::SessionHeader) -> Value {
    let cwd = (header.coordinates().location()
        == &rsi_execution_protocol::ExecutionLocation::Local)
        .then(|| header.canonical_cwd());
    json!({"id":header.session_id(),"created_at":header.created_at_ms(),"cwd":cwd,
        "preset":header.agent_preset_id(),"inherited":header.history_origin().is_some(),
        "parent":header.history_origin().map(|origin| &origin.parent_session_id)})
}

async fn current_selection(
    handle: &dyn SessionHandle,
    header: &rsi_agent_session_protocol::SessionHeader,
) -> Result<(
    rsi_agent_session_protocol::ModelSelection,
    rsi_agent_session_protocol::ProjectionCursor,
)> {
    let mut stream = handle
        .observe_projections()
        .await
        .map_err(|error| remote(&error))?;
    let retained = stream
        .next()
        .await
        .ok_or(Error::Disconnected)?
        .map_err(|error| remote(&error))?;
    let snapshot = retained.snapshot();
    let selection = projected_selection(snapshot.entries())?.unwrap_or_else(|| {
        rsi_agent_session_protocol::ModelSelection {
            model: header.settings().default_model().clone(),
            reasoning_effort: header.settings().default_reasoning_effort().cloned(),
        }
    });
    Ok((selection, snapshot.cursor()))
}

fn projected_selection(
    entries: &[rsi_agent_session_protocol::ProjectionEntry],
) -> Result<Option<rsi_agent_session_protocol::ModelSelection>> {
    let Some(entry) = entries
        .iter()
        .find(|entry| entry.producer().as_str() == rsi_agent_model_selection::VIEW)
    else {
        return Ok(None);
    };
    let value = entry.view().ok_or_else(|| {
        Error::Remote(
            entry
                .failure()
                .unwrap_or("Model selection unavailable")
                .into(),
        )
    })?;
    serde_json::from_value(value.value().clone())
        .map(Some)
        .map_err(|_| Error::Protocol)
}

pub(super) fn resource_key(kind: &str, value: &str) -> Result<String> {
    if value.is_empty()
        || value.len() > 64
        || !value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-')
    {
        return Err(Error::Protocol);
    }
    Ok(format!("{kind}:{value}"))
}
fn remote(error: &rsi_session_protocol::SessionError) -> Error {
    Error::Remote(error.to_string().chars().take(1000).collect())
}

async fn read(
    handle: &dyn SessionHandle,
    after: u64,
    offset: usize,
    attempt: bool,
) -> Result<Value> {
    let before = after.checked_add(2).ok_or(Error::Protocol)?;
    let page = handle
        .history_before(Some(before), 1)
        .await
        .map_err(|error| remote(&error))?;
    let [fact] = page.facts.as_slice() else {
        return Err(Error::Protocol);
    };
    fact_fragment(fact, after, offset, attempt)
}
pub(super) fn fact_fragment(
    fact: &rsi_agent_session_protocol::SessionFact,
    after: u64,
    offset: usize,
    attempt: bool,
) -> Result<Value> {
    if after.checked_add(1) != Some(fact.seq()) {
        return Err(Error::Protocol);
    }
    let encoded = if attempt {
        rsi_extension_agent::project_model_attempt(fact, after)?
    } else {
        serde_json::to_string(std::slice::from_ref(fact)).map_err(|_| Error::Protocol)?
    };
    fragment(&encoded, offset, fact.seq())
}

pub(super) fn fragment(encoded: &str, offset: usize, through: u64) -> Result<Value> {
    let (end, part) = fragment_text(encoded, offset)?;
    Ok(
        json!({"through_seq":through,"total_bytes":encoded.len(),"offset":offset,"next_offset":end,"json":part}),
    )
}

pub(super) fn fragment_text(encoded: &str, offset: usize) -> Result<(usize, &str)> {
    if offset >= encoded.len() || !encoded.is_char_boundary(offset) {
        return Err(Error::Protocol);
    }
    let mut end = offset.saturating_add(128 * 1024).min(encoded.len());
    while !encoded.is_char_boundary(end) {
        end -= 1;
    }
    Ok((end, &encoded[offset..end]))
}

async fn wait(handle: &dyn SessionHandle, mut cursor: ObservationCursor) -> Result<Value> {
    let mut observations = handle
        .observe(cursor)
        .await
        .map_err(|error| remote(&error))?;
    while let Some(item) = observations.next().await {
        match item.map_err(|error| remote(&error))? {
            SessionObservation::Control {
                durable_control_seq,
                ..
            } if durable_control_seq > cursor.control_seq => {
                cursor.control_seq = durable_control_seq;
                return Ok(json!(cursor));
            }
            SessionObservation::Fact {
                durable_fact_seq, ..
            } if durable_fact_seq > cursor.fact_seq => {
                cursor.fact_seq = durable_fact_seq;
                return Ok(json!(cursor));
            }
            SessionObservation::Fact { .. } | SessionObservation::Control { .. } => {}
        }
    }
    Err(Error::Disconnected)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_projection_retains_effort_absence_and_does_not_default_a_failed_producer() {
        use rsi_agent_session_protocol::{ContributionId, ProjectionEntry, ProjectionValue};
        let producer = ContributionId::new(rsi_agent_model_selection::VIEW).unwrap();
        assert!(projected_selection(&[]).unwrap().is_none());
        let selection = json!({"model":{"deployment":"native","model":"selected"}});
        let row = ProjectionEntry::value(
            producer.clone(),
            ProjectionValue::encode(&selection).unwrap(),
        );
        let parsed = projected_selection(&[row]).unwrap().unwrap();
        assert_eq!(parsed.model.model(), "selected");
        assert!(parsed.reasoning_effort.is_none());
        let failed = ProjectionEntry::failed(producer, "state unavailable").unwrap();
        assert!(
            matches!(projected_selection(&[failed]), Err(Error::Remote(message)) if message == "state unavailable")
        );
    }

    #[test]
    fn history_fragments_preserve_multibyte_text_and_reject_invalid_offsets() {
        let prefix = "x".repeat(128 * 1024 - 1);
        let data = format!("{prefix}历史");
        let first = fragment(&data, 0, 7).unwrap();
        assert_eq!(first["json"], prefix);
        let next = prefix.len();
        let last = fragment(&data, next, 7).unwrap();
        assert_eq!(last["json"], "历史");
        assert_eq!(last["next_offset"], data.len());
        assert!(fragment(&data, next + 1, 7).is_err());
        assert!(fragment(&data, data.len(), 7).is_err());
    }
}
