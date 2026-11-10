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
    Capture {
        reader: String,
        after: ObservationCursor,
        offset: usize,
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
        let reader = HistoryReader {
            sessions: self,
            service: service.as_ref(),
            request: &input.request,
            session: &input.session_id,
            parent: input.parent_session_id.as_ref(),
        };
        let result = reader.execute(input.operation).await?;
        self.routes
            .caller(&input.request)
            .map_err(|_| Error::Cancelled)?;
        Ok(result)
    }
}

struct HistoryReader<'a> {
    sessions: &'a Sessions,
    service: &'a dyn SessionService,
    request: &'a str,
    session: &'a SessionId,
    parent: Option<&'a SessionId>,
}

impl HistoryReader<'_> {
    async fn execute(&self, operation: Operation) -> Result<Value> {
        match operation {
            Operation::Capture {
                reader,
                after,
                offset,
            } => self.capture(&reader, after, offset).await,
            Operation::Open { metadata_only } => {
                open_history(self.service, self.session, self.parent, metadata_only).await
            }
            Operation::Controls {
                reader,
                after_seq,
                through_seq,
                offset,
            } => self.controls(&reader, after_seq, through_seq, offset).await,
            Operation::Message {
                reader,
                message_id,
                accepted_control_seq,
                offset,
            } => {
                self.message(&reader, &message_id, accepted_control_seq, offset)
                    .await
            }
            Operation::Fact {
                reader,
                after_seq,
                through_seq,
                offset,
            } => self.facts(&reader, after_seq, through_seq, offset).await,
            Operation::PageClose { reader } => self.close("history", &reader),
            Operation::SeedOpen { key } => {
                let handle = self.attach().await?;
                self.sessions
                    .open_inherited(
                        self.request,
                        &key,
                        &handle.header().await.map_err(|error| remote(&error))?,
                    )
                    .await
            }
            Operation::SeedRead(read) => {
                let _handle = self.attach().await?;
                self.sessions
                    .read_inherited(self.request, self.session, read)
                    .await
            }
            Operation::SeedClose { key } => {
                let _handle = self.attach().await?;
                self.close("seed", &key)
            }
            Operation::Wait { cursor } => {
                let handle = self.attach().await?;
                wait(
                    handle.as_ref(),
                    self.sessions.store.as_ref(),
                    self.session,
                    cursor,
                )
                .await
            }
            Operation::ModelOpen { key, effect } => {
                let handle = self.attach().await?;
                self.sessions
                    .open_model(
                        self.request,
                        &key,
                        self.session.clone(),
                        effect,
                        handle.as_ref(),
                    )
                    .await
            }
            Operation::ModelNext { key } => {
                let _handle = self.attach().await?;
                self.sessions
                    .next_model(self.request, &key, self.session)
                    .await
            }
            Operation::ModelClose { key } => {
                let _handle = self.attach().await?;
                self.close("model", &key)
            }
        }
    }

    async fn attach(&self) -> Result<std::sync::Arc<dyn SessionHandle>> {
        find_handle(self.service, self.session, self.parent)
            .await?
            .ok_or_else(unavailable)
    }

    async fn header(&self) -> Result<SessionHeader> {
        find_summary(self.service, self.session, self.parent)
            .await?
            .map(|summary| summary.header)
            .ok_or_else(unavailable)
    }

    fn close(&self, kind: &str, key: &str) -> Result<Value> {
        self.sessions
            .routes
            .release_resource(self.request, &resource_key(kind, key)?);
        Ok(Value::Null)
    }

    async fn capture(
        &self,
        reader: &str,
        after: ObservationCursor,
        offset: usize,
    ) -> Result<Value> {
        self.sessions
            .history_pages
            .read(
                &self.sessions.routes,
                self.request,
                reader,
                offset,
                Identity::Capture {
                    session: self.session.clone(),
                    parent: self.parent.cloned(),
                    fact: after.fact_seq,
                    control: after.control_seq,
                },
                super::session_capture::capture(
                    self.sessions.store.as_ref(),
                    self.service,
                    self.session,
                    self.parent,
                    after,
                ),
            )
            .await
    }

    async fn controls(
        &self,
        reader: &str,
        after: u64,
        through: u64,
        offset: usize,
    ) -> Result<Value> {
        self.sessions
            .history_pages
            .read(
                &self.sessions.routes,
                self.request,
                reader,
                offset,
                Identity::Controls {
                    session: self.session.clone(),
                    parent: self.parent.cloned(),
                    after,
                    through,
                },
                async {
                    let header = self.header().await?;
                    super::session_controls::read_controls(
                        self.sessions.store.as_ref(),
                        &header,
                        after,
                        through,
                    )
                    .await
                },
            )
            .await
    }

    async fn message(
        &self,
        reader: &str,
        message: &rsi_agent_session_protocol::MessageId,
        accepted: u64,
        offset: usize,
    ) -> Result<Value> {
        self.sessions
            .history_pages
            .read(
                &self.sessions.routes,
                self.request,
                reader,
                offset,
                Identity::Message {
                    session: self.session.clone(),
                    parent: self.parent.cloned(),
                    message: message.clone(),
                    accepted,
                },
                async {
                    let header = self.header().await?;
                    read_message(
                        self.sessions.store.as_ref(),
                        header.session_id(),
                        message,
                        accepted,
                    )
                    .await
                },
            )
            .await
    }

    async fn facts(&self, reader: &str, after: u64, through: u64, offset: usize) -> Result<Value> {
        self.sessions
            .history_pages
            .read(
                &self.sessions.routes,
                self.request,
                reader,
                offset,
                Identity::Facts {
                    session: self.session.clone(),
                    parent: self.parent.cloned(),
                    after,
                    through,
                },
                async {
                    let header = self.header().await?;
                    read_facts(
                        self.sessions.store.as_ref(),
                        header.session_id(),
                        after,
                        through,
                    )
                    .await
                },
            )
            .await
    }
}

async fn open_history(
    service: &dyn SessionService,
    session: &SessionId,
    parent: Option<&SessionId>,
    metadata_only: bool,
) -> Result<Value> {
    let metadata = metadata_snapshot(service, session, parent).await?;
    if metadata_only || !metadata.is_null() {
        return Ok(metadata);
    }
    let Some(handle) = find_handle(service, session, parent).await? else {
        return Ok(Value::Null);
    };
    let header = handle.header().await.map_err(|error| remote(&error))?;
    let (selection, cursor) = current_selection(handle.as_ref(), &header).await?;
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

pub(super) async fn find_summary(
    service: &dyn SessionService,
    session: &SessionId,
    parent: Option<&SessionId>,
) -> Result<Option<rsi_session_protocol::SessionSummary>> {
    let summary = match service.read_summary(session).await {
        Ok(summary) => summary,
        Err(
            rsi_session_protocol::SessionError::NotFound(_)
            | rsi_session_protocol::SessionError::Api(rsi_api_protocol::ApiError::Unauthorized),
        ) => return Ok(None),
        Err(_) => return Err(unavailable()),
    };
    if let Some(parent) = parent {
        check_parent(service, &summary.header, parent).await?;
    }
    Ok(Some(summary))
}

async fn check_parent(
    service: &dyn SessionService,
    header: &SessionHeader,
    parent: &SessionId,
) -> Result<()> {
    service
        .read_summary(parent)
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
    session: &SessionId,
    parent: Option<&SessionId>,
) -> Result<Value> {
    let Some(summary) = find_summary(service, session, parent).await? else {
        return Ok(Value::Null);
    };
    Ok(stored_history(
        &summary.header,
        summary.durable_fact_seq,
        summary.durable_control_seq,
    ))
}

pub(super) fn stored_history(header: &SessionHeader, fact_seq: u64, control_seq: u64) -> Value {
    let mut metadata = stored_snapshot(header, fact_seq, control_seq);
    metadata["through"] = json!(fact_seq);
    metadata["model_baseline"] = json!(model_baseline(header));
    metadata
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
    rsi_extension_agent::project_history_page(
        &page.facts,
        last,
        rsi_agent_session_protocol::MAXIMUM_SESSION_FACT_BYTES + 2,
    )
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
