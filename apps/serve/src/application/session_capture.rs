//! A bounded first read over one freshly authorized durable cut.
use super::session_history::{HISTORY_PAGE_BYTES, find_summary, stored_history};
use rsi_agent_session_protocol::{SessionHeader, SessionId};
use rsi_agent_store_protocol::{SessionStore, StoreError};
use rsi_agent_turn_protocol::ObservationCursor;
use rsi_extension::{Error, Result};
use rsi_session_protocol::SessionService;
use std::fmt::Write;

type Slice = Option<(u64, String)>;

pub(super) async fn capture(
    store: &dyn SessionStore,
    service: &dyn SessionService,
    session: &SessionId,
    parent: Option<&SessionId>,
    after: ObservationCursor,
) -> Result<(u64, String)> {
    let Some(summary) = find_summary(service, session, parent).await? else {
        return Ok((0, "null".into()));
    };
    let fact = summary.durable_fact_seq;
    let control = summary.durable_control_seq;
    if after.fact_seq > fact || after.control_seq > control {
        return Err(Error::Protocol);
    }
    let meta = serde_json::to_string(&stored_history(&summary.header, fact, control))
        .map_err(|_| Error::Protocol)?;
    // Two u64 slice positions and the fixed JSON wrappers fit within 128 bytes.
    let available = HISTORY_PAGE_BYTES
        .checked_sub(meta.len() + 128)
        .ok_or(Error::Capacity)?;
    let streams = usize::from(after.fact_seq < fact) + usize::from(after.control_seq < control);
    let budget = available / streams.max(1);
    let (facts, controls) = tokio::try_join!(
        facts(store, session, after.fact_seq, fact, budget),
        controls(store, &summary.header, after.control_seq, control, budget),
    )?;
    let length = |slice: &Slice| slice.as_ref().map_or(4, |(_, json)| json.len());
    let mut encoded = String::with_capacity(
        (meta.len() + length(&facts) + length(&controls) + 128).min(HISTORY_PAGE_BYTES),
    );
    write!(encoded, "{{\"meta\":{meta},\"facts\":").map_err(|_| Error::Protocol)?;
    write_slice(&mut encoded, facts)?;
    encoded.push_str(",\"controls\":");
    write_slice(&mut encoded, controls)?;
    encoded.push('}');
    Ok((fact, encoded))
}

fn write_slice(output: &mut String, value: Slice) -> Result<()> {
    match value {
        Some((through, json)) => write!(output, "{{\"through\":{through},\"value\":{json}}}")
            .map_err(|_| Error::Protocol),
        None => {
            output.push_str("null");
            Ok(())
        }
    }
}

async fn facts(
    store: &dyn SessionStore,
    session: &SessionId,
    after: u64,
    through: u64,
    budget: usize,
) -> Result<Slice> {
    if after == through || budget < 2 {
        return Ok(None);
    }
    let page = match store
        .read_facts(session, after, (through - after).min(512) as usize, budget)
        .await
    {
        Err(StoreError::ReadRequiresCapacity { .. }) => return Ok(None),
        result => result.map_err(|error| Error::Remote(error.to_string()))?,
    };
    if through > page.durable_seq {
        return Err(Error::Protocol);
    }
    let last = page.facts.last().ok_or(Error::Protocol)?.seq();
    match rsi_extension_agent::project_history_page(&page.facts, last, budget) {
        Err(Error::Capacity) => Ok(None),
        result => result.map(Some),
    }
}

async fn controls(
    store: &dyn SessionStore,
    header: &SessionHeader,
    after: u64,
    through: u64,
    budget: usize,
) -> Result<Slice> {
    if after == through || budget < 2 {
        return Ok(None);
    }
    let page = match store
        .read_controls(
            header.session_id(),
            after,
            (through - after).min(64) as usize,
            budget,
        )
        .await
    {
        Err(StoreError::ReadRequiresCapacity { .. }) => return Ok(None),
        result => result.map_err(|error| Error::Remote(error.to_string()))?,
    };
    let value = super::session_controls::encode_controls(header, page, after, through)?;
    Ok((value.1.len() <= budget).then_some(value))
}
