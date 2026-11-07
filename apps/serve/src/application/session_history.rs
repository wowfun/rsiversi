use super::{Sessions, session_fragments::Identity};
use futures_util::StreamExt;
use rsi_agent_session_protocol::{SessionHeader, SessionId};
use rsi_agent_store_protocol::{SessionStore, StoreError};
use rsi_agent_turn_protocol::{ObservationCursor, SessionObservation};
use rsi_extension::{Error, Result};
use rsi_session_protocol::{SessionHandle, SessionService};
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
        reader: String,
        after_seq: u64,
        through_seq: u64,
        offset: usize,
    },
    Message {
        reader: String,
        message_id: rsi_agent_session_protocol::MessageId,
        accepted_control_seq: u64,
        offset: usize,
    },
    Fact {
        reader: String,
        after_seq: u64,
        through_seq: u64,
        offset: usize,
    },
    PageClose {
        reader: String,
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
        let origin = self.routes.caller(request).map_err(|_| Error::Cancelled)?;
        let service = self.ingress.scoped(origin);
        find_handle(service.as_ref(), session, parent)
            .await?
            .ok_or_else(unavailable)
    }

    pub(super) async fn history(&self, payload: Value) -> Result<Value> {
        let input: HistoryRead = serde_json::from_value(payload).map_err(|_| Error::Protocol)?;
        let origin = self
            .routes
            .caller(&input.request)
            .map_err(|_| Error::Cancelled)?;
        let service = self.ingress.scoped(origin);
        let store = self.store.as_ref();
        let find = || {
            find_handle(
                service.as_ref(),
                &input.session_id,
                input.parent_session_id.as_ref(),
            )
        };
        let attach = || async { find().await?.ok_or_else(unavailable) };
        let header = || async {
            find_header(
                service.as_ref(),
                &input.session_id,
                input.parent_session_id.as_ref(),
            )
            .await?
            .ok_or_else(unavailable)
        };
        let result = match input.operation {
            Operation::Open {
                metadata_only: true,
            } => {
                metadata_snapshot(
                    service.as_ref(),
                    store,
                    &input.session_id,
                    input.parent_session_id.as_ref(),
                )
                .await?
            }
            Operation::Open {
                metadata_only: false,
            } => match find().await? {
                Some(handle) => self.opening_snapshot(handle.as_ref()).await?,
                None => Value::Null,
            },
            Operation::Controls {
                reader,
                after_seq,
                through_seq,
                offset,
            } => {
                self.history_pages
                    .read(
                        &self.routes,
                        &input.request,
                        &reader,
                        offset,
                        Identity::Controls {
                            session: input.session_id.clone(),
                            parent: input.parent_session_id.clone(),
                            after: after_seq,
                            through: through_seq,
                        },
                        async {
                            let header = header().await?;
                            super::session_controls::read_controls(
                                store,
                                &header,
                                after_seq,
                                through_seq,
                            )
                            .await
                        },
                    )
                    .await?
            }
            Operation::Message {
                reader,
                message_id,
                accepted_control_seq,
                offset,
            } => {
                self.history_pages
                    .read(
                        &self.routes,
                        &input.request,
                        &reader,
                        offset,
                        Identity::Message {
                            session: input.session_id.clone(),
                            parent: input.parent_session_id.clone(),
                            message: message_id.clone(),
                            accepted: accepted_control_seq,
                        },
                        async {
                            let header = header().await?;
                            read_message(
                                store,
                                header.session_id(),
                                &message_id,
                                accepted_control_seq,
                            )
                            .await
                        },
                    )
                    .await?
            }
            Operation::Fact {
                reader,
                after_seq,
                through_seq,
                offset,
            } => {
                self.history_pages
                    .read(
                        &self.routes,
                        &input.request,
                        &reader,
                        offset,
                        Identity::Facts {
                            session: input.session_id.clone(),
                            parent: input.parent_session_id.clone(),
                            after: after_seq,
                            through: through_seq,
                        },
                        async {
                            let header = header().await?;
                            read_facts(store, header.session_id(), after_seq, through_seq).await
                        },
                    )
                    .await?
            }
            Operation::PageClose { reader } => {
                self.routes
                    .release_resource(&input.request, &resource_key("history", &reader)?);
                Value::Null
            }
            Operation::SeedOpen { key } => {
                let handle = attach().await?;
                self.open_inherited(
                    &input.request,
                    &key,
                    &handle.header().await.map_err(|error| remote(&error))?,
                )
                .await?
            }
            Operation::SeedRead(read) => {
                let _handle = attach().await?;
                self.read_inherited(&input.request, &input.session_id, read)
                    .await?
            }
            Operation::SeedClose { key } => {
                let _handle = attach().await?;
                self.routes
                    .release_resource(&input.request, &resource_key("seed", &key)?);
                Value::Null
            }
            Operation::Wait { cursor } => {
                let handle = attach().await?;
                wait(handle.as_ref(), store, &input.session_id, cursor).await?
            }
            Operation::ModelOpen { key, effect } => {
                let handle = attach().await?;
                self.open_model(
                    &input.request,
                    &key,
                    input.session_id.clone(),
                    effect,
                    handle.as_ref(),
                )
                .await?
            }
            Operation::ModelNext { key } => {
                let _handle = attach().await?;
                self.next_model(&input.request, &key, &input.session_id)
                    .await?
            }
            Operation::ModelClose { key } => {
                let _handle = attach().await?;
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

    async fn opening_snapshot(&self, handle: &dyn SessionHandle) -> Result<Value> {
        let header = handle.header().await.map_err(|error| remote(&error))?;
        let (selection, cursor) = current_selection(handle, &header).await?;
        let through = match cursor {
            rsi_agent_session_protocol::ProjectionCursor::Durable { fact_seq, .. } => fact_seq,
            rsi_agent_session_protocol::ProjectionCursor::Draft { .. } => 0,
        };
        let mut metadata = stored_header(&header);
        metadata["through"] = json!(through);
        metadata["model_selection"] = json!(selection);
        metadata["selection_cursor"] = json!(cursor);
        metadata["model_baseline"] = json!(model_baseline(&header));
        Ok(metadata)
    }
}

fn unavailable() -> Error {
    Error::Remote("Session unavailable to caller".into())
}

async fn find_handle(
    service: &dyn SessionService,
    session: &SessionId,
    parent: Option<&SessionId>,
) -> Result<Option<std::sync::Arc<dyn SessionHandle>>> {
    let handle = match service.attach(session).await {
        Ok(handle) => handle,
        Err(
            rsi_session_protocol::SessionError::NotFound(_)
            | rsi_session_protocol::SessionError::Api(rsi_api_protocol::ApiError::Unauthorized),
        ) => return Ok(None),
        Err(_) => return Err(unavailable()),
    };
    if let Some(parent) = parent {
        check_parent(
            service,
            &handle.header().await.map_err(|error| remote(&error))?,
            parent,
        )
        .await?;
    }
    Ok(Some(handle))
}

async fn find_header(
    service: &dyn SessionService,
    session: &SessionId,
    parent: Option<&SessionId>,
) -> Result<Option<SessionHeader>> {
    let header = match service.read_header(session).await {
        Ok(header) => header,
        Err(
            rsi_session_protocol::SessionError::NotFound(_)
            | rsi_session_protocol::SessionError::Api(rsi_api_protocol::ApiError::Unauthorized),
        ) => return Ok(None),
        Err(_) => return Err(unavailable()),
    };
    if let Some(parent) = parent {
        check_parent(service, &header, parent).await?;
    }
    Ok(Some(header))
}

async fn check_parent(
    service: &dyn SessionService,
    header: &SessionHeader,
    parent: &SessionId,
) -> Result<()> {
    service
        .read_header(parent)
        .await
        .map_err(|_| Error::Remote("Parent unavailable to caller".into()))?;
    if header
        .fork_origin()
        .map(|fork| &fork.history.parent_session_id)
        != Some(parent)
    {
        return Err(Error::Remote(
            "Session address does not identify a direct child".into(),
        ));
    }
    Ok(())
}

async fn metadata_snapshot(
    service: &dyn SessionService,
    store: &dyn SessionStore,
    session: &SessionId,
    parent: Option<&SessionId>,
) -> Result<Value> {
    let Some(header) = find_header(service, session, parent).await? else {
        return Ok(Value::Null);
    };
    let rows = store
        .session_activity_summaries(std::slice::from_ref(header.session_id()))
        .await
        .map_err(|_| Error::Remote("Session metadata unavailable".into()))?;
    match rows.as_slice() {
        [Some(row)]
            if row.session_id == *header.session_id()
                && row.created_at_ms == header.created_at_ms() =>
        {
            let mut metadata =
                stored_snapshot(&header, row.durable_fact_seq, row.durable_control_seq);
            metadata["through"] = json!(row.durable_fact_seq);
            metadata["model_baseline"] = json!(model_baseline(&header));
            Ok(metadata)
        }
        [None] => Ok(Value::Null),
        _ => Err(Error::Remote("Session catalog changed during read".into())),
    }
}

pub(super) fn stored_snapshot(
    header: &rsi_agent_session_protocol::SessionHeader,
    fact_seq: u64,
    control_seq: u64,
) -> Value {
    let mut item = stored_header(header);
    item["selection_cursor"] = json!(rsi_agent_session_protocol::ProjectionCursor::Durable {
        fact_seq,
        control_seq,
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
    let selection =
        projected_selection(snapshot.entries())?.unwrap_or_else(|| model_baseline(header));
    Ok((selection, snapshot.cursor()))
}

fn model_baseline(header: &SessionHeader) -> rsi_agent_session_protocol::ModelSelection {
    rsi_agent_session_protocol::ModelSelection {
        model: header.settings().default_model().clone(),
        reasoning_effort: header.settings().default_reasoning_effort().cloned(),
    }
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

pub(super) async fn read_message(
    store: &dyn SessionStore,
    session: &SessionId,
    message: &rsi_agent_session_protocol::MessageId,
    accepted_control_seq: u64,
) -> Result<(u64, String)> {
    let after = accepted_control_seq.checked_sub(1).ok_or(Error::Protocol)?;
    let page = store
        .read_controls(
            session,
            after,
            1,
            rsi_agent_store_protocol::MAXIMUM_STORE_CONTROL_PAGE_BYTES,
        )
        .await
        .map_err(|error| Error::Remote(error.to_string()))?;
    if let Some(record) = page.records.into_iter().next()
        && record.seq() == accepted_control_seq
        && let rsi_agent_session_protocol::AgentControlRecordBody::MessageAccepted {
            message: accepted,
            ..
        } = record.into_body()
        && accepted.message_id == *message
    {
        let encoded = serde_json::to_string(&accepted).map_err(|_| Error::Protocol)?;
        return Ok((accepted_control_seq, encoded));
    }
    Err(Error::Remote("exact message acceptance unavailable".into()))
}

/// The Store bounds body allocation before decoding. Oversized individual Facts
/// retain the existing fragmented transport, without widening ordinary pages.
pub(super) async fn read_facts(
    store: &dyn SessionStore,
    session: &SessionId,
    after: u64,
    through: u64,
) -> Result<(u64, String)> {
    let count = through
        .checked_sub(after)
        .filter(|count| *count > 0)
        .ok_or(Error::Protocol)?;
    let limit = count.min(512) as usize;
    let page = match store
        .read_facts(session, after, limit, HISTORY_PAGE_BYTES)
        .await
    {
        Err(StoreError::ReadRequiresCapacity { minimum_bytes }) => {
            store.read_facts(session, after, 1, minimum_bytes).await
        }
        result => result,
    }
    .map_err(|error| Error::Remote(error.to_string()))?;
    if through > page.durable_seq {
        return Err(Error::Protocol);
    }
    let last = page.facts.last().ok_or(Error::Protocol)?.seq();
    rsi_extension_agent::project_history_page(&page.facts, last)
}

pub(super) fn fragment(encoded: &str, offset: usize, through: u64) -> Result<Value> {
    let (end, part) = fragment_text(encoded, offset)?;
    Ok(
        json!({"through_seq":through,"total_bytes":encoded.len(),"offset":offset,"next_offset":end,"json":part}),
    )
}

pub(super) const HISTORY_PAGE_BYTES: usize = rsi_extension::MAXIMUM_FRAME_BYTES;
const FRAGMENT_JSON_BYTES: usize = rsi_extension::MAXIMUM_FRAME_BYTES - 8 * 1024;

pub(super) fn fragment_text(encoded: &str, offset: usize) -> Result<(usize, &str)> {
    if offset >= encoded.len() || !encoded.is_char_boundary(offset) {
        return Err(Error::Protocol);
    }
    // Any byte takes at most six bytes in a JSON string. Small pages fit without
    // scanning; larger pages count the same escapes as serde_json's formatter.
    if encoded.len() - offset <= FRAGMENT_JSON_BYTES / 6 {
        return Ok((encoded.len(), &encoded[offset..]));
    }
    let mut end = offset;
    let mut wire_bytes = 0;
    for byte in &encoded.as_bytes()[offset..] {
        let width = match byte {
            b'"' | b'\\' | b'\x08' | b'\t' | b'\n' | b'\x0c' | b'\r' => 2,
            0..=0x1f => 6,
            _ => 1,
        };
        if wire_bytes + width > FRAGMENT_JSON_BYTES {
            break;
        }
        wire_bytes += width;
        end += 1;
    }
    while !encoded.is_char_boundary(end) {
        end -= 1;
    }
    Ok((end, &encoded[offset..end]))
}

async fn wait(
    handle: &dyn SessionHandle,
    store: &dyn SessionStore,
    session: &SessionId,
    cursor: ObservationCursor,
) -> Result<Value> {
    let mut observations = handle
        .observe(cursor)
        .await
        .map_err(|error| remote(&error))?;
    while let Some(item) = observations.next().await {
        let advanced = match item.map_err(|error| remote(&error))? {
            SessionObservation::Control {
                durable_control_seq,
                ..
            } => durable_control_seq > cursor.control_seq,
            SessionObservation::Fact {
                durable_fact_seq, ..
            } => durable_fact_seq > cursor.fact_seq,
        };
        if advanced {
            let current = store
                .read_watermarks(session)
                .await
                .map_err(|error| Error::Remote(error.to_string()))?;
            return Ok(
                json!({"control_seq":current.durable_control_seq,"fact_seq":current.durable_fact_seq}),
            );
        }
    }
    Err(Error::Disconnected)
}

#[cfg(test)]
mod tests;
