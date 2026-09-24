//! Fixed-cut, backpressured Session artifacts.
#![deny(unsafe_code)]
mod diagnostic;
mod native;
mod projection;
#[cfg(test)]
mod tests;
use futures_util::{Stream, StreamExt};
pub use native::{write_file, write_stream};
use rsi_agent_session_protocol::{SessionFact, SessionHeader, SessionId};
use rsi_agent_store_protocol::{SessionStore, SessionValidationLease, StoreError};
use rsi_session_protocol::{
    Result, SessionError,
    export::{
        CHUNK_BYTES, ExportEvent, ExportFormat, ExportInclude, ExportOptions, ExportStream,
        ExportVerifier, default_filename,
    },
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{pin::Pin, sync::Arc};
use tokio_util::sync::CancellationToken;

type TextStream = Pin<Box<dyn Stream<Item = Result<String>> + Send>>;
type FactStream = Pin<Box<dyn Stream<Item = Result<SessionFact>> + Send>>;

#[derive(Clone)]
struct Interval {
    id: SessionId,
    after: u64,
    through: u64,
}
struct Cut {
    store: Option<Arc<dyn SessionStore>>,
    header: SessionHeader,
    through: u64,
    intervals: Vec<Interval>,
    _leases: Vec<SessionValidationLease>,
}

impl Cut {
    fn store(&self) -> Arc<dyn SessionStore> {
        self.store
            .as_ref()
            .expect("only durable intervals read a Store")
            .clone()
    }
}

/// Exports a frozen, unpublished draft without creating durable state.
///
/// # Errors
/// Returns an error for invalid options or an unencodable Header.
pub fn empty(
    header: SessionHeader,
    options: ExportOptions,
    stopped: CancellationToken,
) -> Result<ExportStream> {
    options.validate()?;
    stream(
        Cut {
            store: None,
            header,
            through: 0,
            intervals: vec![],
            _leases: vec![],
        },
        options,
        stopped,
    )
}

/// Captures a durable cut before returning a lazy artifact stream.
///
/// # Errors
/// Returns an error for invalid options, unavailable history, or a changed Header or fork binding.
pub async fn export(
    store: Arc<dyn SessionStore>,
    header: SessionHeader,
    options: ExportOptions,
    stopped: CancellationToken,
) -> Result<ExportStream> {
    options.validate()?;
    let lease = store
        .prepare_session(header.session_id())
        .await
        .map_err(store_error)?;
    let actual = store
        .header(header.session_id())
        .await
        .map_err(store_error)?;
    if actual != header {
        return Err(invalid("export Header changed"));
    }
    let watermarks = store
        .read_watermarks(header.session_id())
        .await
        .map_err(store_error)?;
    let through = watermarks.durable_fact_seq;
    let mut intervals = vec![];
    let mut leases = vec![lease];
    if let Some(origin) = header.fork_origin() {
        leases.push(
            store
                .prepare_session(&origin.parent_session_id)
                .await
                .map_err(store_error)?,
        );
        let parent = store
            .header(&origin.parent_session_id)
            .await
            .map_err(store_error)?;
        let boundary = store
            .resolve_fork_boundary(
                &origin.parent_session_id,
                &origin.invoking_turn_id,
                origin.requested_turns.clone(),
            )
            .await
            .map_err(store_error)?;
        if parent.fingerprint().map_err(encoding)? != origin.parent_header_fingerprint
            || boundary.resolved_after_seq != origin.resolved_after_seq
            || boundary.resolved_terminal_seq != origin.resolved_terminal_seq
            || boundary.terminal_prefix_sha256 != origin.terminal_prefix_sha256
            || boundary.resolved_terminal_control_seq != origin.resolved_terminal_control_seq
            || boundary.terminal_control_prefix_sha256 != origin.terminal_control_prefix_sha256
            || boundary.effective_turns != origin.effective_turns
        {
            return Err(invalid("export inherited history binding changed"));
        }
        intervals.push(Interval {
            id: origin.parent_session_id.clone(),
            after: origin.resolved_after_seq,
            through: origin.resolved_terminal_seq,
        });
    }
    intervals.push(Interval {
        id: header.session_id().clone(),
        after: 0,
        through,
    });
    stream(
        Cut {
            store: Some(store),
            header,
            through,
            intervals,
            _leases: leases,
        },
        options,
        stopped,
    )
}
fn stream(cut: Cut, options: ExportOptions, stopped: CancellationToken) -> Result<ExportStream> {
    let header = &cut.header;
    let start = ExportEvent::Start {
        session_id: header.session_id().clone(),
        header_sha256: header.fingerprint().map_err(encoding)?,
        through_seq: cut.through.to_string(),
        filename: default_filename(header.session_id(), options.format),
        options: options.clone(),
    };
    let cut = Arc::new(cut);
    let source = async_stream::try_stream! {
        yield start;
        let mut source = document(cut, options);
        let mut hash = Sha256::new();
        let mut offset = 0u64;
        while let Some(text) = source.next().await {
            let text = text?;
            let mut remaining = text.as_str();
            while !remaining.is_empty() {
                let mut end = remaining.len().min(CHUNK_BYTES);
                while !remaining.is_char_boundary(end) { end -= 1; }
                let chunk = &remaining[..end];
                let event = ExportEvent::Chunk { offset: offset.to_string(), text: chunk.into() };
                hash.update(chunk.as_bytes());
                offset = offset.checked_add(end as u64).ok_or_else(|| invalid("export length overflow"))?;
                yield event;
                remaining = &remaining[end..];
            }
        }
        yield ExportEvent::Complete { bytes: offset.to_string(), sha256: hex::encode(hash.finalize()) };
    };
    let mut source = Box::pin(source);
    Ok(Box::pin(async_stream::try_stream! {
        loop {
            let item = tokio::select! { biased;
                () = stopped.cancelled() => Some(Err(SessionError::Backend("Session export stopped".into()))),
                item = source.next() => item,
            };
            let Some(item) = item else { break; };
            yield item?;
        }
    }))
}

fn facts(store: Arc<dyn SessionStore>, interval: Interval) -> FactStream {
    Box::pin(async_stream::try_stream! {
        let mut after = interval.after;
        while after < interval.through {
            let limit = usize::try_from((interval.through - after).min(32)).expect("bounded page");
            let page = store.read_facts(&interval.id, after, limit).await.map_err(store_error)?;
            if page.facts.is_empty() { Err(invalid("export history made no progress"))?; }
            for fact in page.facts {
                if fact.seq() != after + 1 || fact.seq() > interval.through { Err(invalid("export history escaped its cut"))?; }
                after = fact.seq();
                yield fact;
            }
        }
    })
}

fn document(cut: Arc<Cut>, options: ExportOptions) -> TextStream {
    Box::pin(async_stream::try_stream! {
        let json_format = options.format == ExportFormat::Json;
        let reasoning = options.has(ExportInclude::Reasoning);
        let latest = if options.has(ExportInclude::LastProviderRequest) || options.has(ExportInclude::LastProviderResponse) {
            diagnostic::latest(&cut).await?
        } else { None };
        if json_format { yield "{\n".into(); }
        let mut first_section = true;
        for (section, name) in [
            (ExportInclude::Header, "header"), (ExportInclude::Messages, "messages"),
            (ExportInclude::ProviderInputEvidence, "provider_input_evidence"),
            (ExportInclude::LastProviderRequest, "last_provider_request"),
            (ExportInclude::LastProviderResponse, "last_provider_response"),
        ] {
            if !options.has(section) { continue; }
            if json_format {
                yield format!("{}  {name:?}: ", if first_section { "" } else { ",\n" });
            } else { yield format!("# {}\n\n", name.replace('_', " ")); }
            first_section = false;
            match section {
                ExportInclude::Header => {
                    let mut header = serde_json::to_value(&cut.header).map_err(encoding)?;
                    if let Some(settings) = header.get_mut("settings").and_then(Value::as_object_mut) { settings.remove("system_prompt"); }
                    if let Some(object) = header.as_object_mut() { object.remove("spawn_role"); object.remove("delegation_policy"); }
                    let value = json!({"session": header,"options":options,"through_seq":cut.through.to_string()});
                    yield render_value(&value, json_format)?;
                }
                ExportInclude::LastProviderRequest => {
                    let value = match &latest { Some(latest) => diagnostic::request(&cut, latest).await?, None => json!({"availability":"unavailable","reason":"no_completed_conversation"}) };
                    yield render_value(&value, json_format)?;
                }
                ExportInclude::Messages | ExportInclude::ProviderInputEvidence | ExportInclude::LastProviderResponse => {
                    if section == ExportInclude::LastProviderResponse {
                        let Some(latest) = &latest else {
                            yield render_value(&json!({"availability":"unavailable","reason":"no_completed_conversation"}), json_format)?;
                            continue;
                        };
                        let metadata = json!({"raw":false,"reconstructed":true,"session_id":latest.id,"effect_id":latest.effect,"intent_seq":latest.intent.to_string(),"completed_seq":latest.end.to_string()});
                        if json_format {
                            let mut text = serde_json::to_string(&metadata).map_err(encoding)?;
                            text.pop();
                            yield format!("{text},\"events\":");
                        } else { yield render_value(&metadata, false)?; }
                    }
                    if json_format { yield "[\n".into(); }
                    let mut first = true;
                    let intervals = if section == ExportInclude::LastProviderResponse {
                        let last = latest.as_ref().expect("checked latest");
                        vec![Interval { id:last.id.clone(), after:last.intent, through:last.end }]
                    } else { cut.intervals.clone() };
                    for interval in intervals {
                        let id = interval.id.clone();
                        let mut source = facts(cut.store(), interval);
                        let mut projection = projection::Projection::default();
                        let mut markdown = projection::Markdown::default();
                        while let Some(fact) = source.next().await {
                            let fact = fact?;
                            let record = if section == ExportInclude::ProviderInputEvidence {
                                diagnostic::evidence_record(&*cut.store(), &id, &fact).await?
                            } else {
                                let effect = (section == ExportInclude::LastProviderResponse).then(|| &latest.as_ref().expect("checked latest").effect);
                                projection.record(&id, &fact, reasoning, effect)?
                            };
                            if let Some(record) = record {
                                if json_format && !first { yield ",\n".into(); }
                                first = false;
                                yield if json_format { serde_json::to_string(&record).map_err(encoding)? } else { markdown.record(&record)? };
                            }
                        }
                        if !json_format { yield markdown.finish().into(); }
                    }
                    if json_format {
                        yield if section == ExportInclude::LastProviderResponse { "\n]}".into() } else { "\n]".into() };
                    }
                }
                ExportInclude::Reasoning => unreachable!("reasoning is a modifier"),
            }
            if !json_format { yield "\n".into(); }
        }
        if json_format { yield "\n}\n".into(); }
    })
}
fn render_value(value: &Value, json_format: bool) -> Result<String> {
    let text = serde_json::to_string_pretty(value).map_err(encoding)?;
    Ok(if json_format {
        text
    } else {
        fenced(&text, "json")
    })
}
fn fenced(text: &str, language: &str) -> String {
    let fence = "`".repeat(
        text.split(|c| c != '`')
            .map(str::len)
            .max()
            .unwrap_or(0)
            .max(2)
            + 1,
    );
    format!("{fence}{language}\n{text}\n{fence}\n\n")
}
fn invalid(message: &str) -> SessionError {
    SessionError::Invalid(message.into())
}
fn encoding(error: impl std::fmt::Display) -> SessionError {
    SessionError::Backend(error.to_string())
}
fn store_error(error: StoreError) -> SessionError {
    match error {
        StoreError::NotFound(_) => SessionError::NotFound("export source".into()),
        error => encoding(error),
    }
}
