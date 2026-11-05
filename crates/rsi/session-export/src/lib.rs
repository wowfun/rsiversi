//! Fixed-cut, backpressured Session artifacts.
#![deny(unsafe_code)]
mod diagnostic;
mod input;
mod native;
mod projection;
#[cfg(test)]
mod tests;
use futures_util::{Stream, StreamExt};
pub use native::{FileWriteError, write_file, write_stream};
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
const FACT_PAGE_COUNT: usize = 32;
type FactStream = Pin<Box<dyn Stream<Item = Result<SessionFact>> + Send>>;

#[derive(Clone)]
struct Interval {
    id: SessionId,
    after: u64,
    through: u64,
}
struct Cut {
    context_budget: rsi_agent_context::ContextBudget,
    store: Option<Arc<dyn SessionStore>>,
    header: SessionHeader,
    through: u64,
    intervals: Vec<Interval>,
    fork_sources: Vec<SessionHeader>,
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
    context_budget: rsi_agent_context::ContextBudget,
) -> Result<ExportStream> {
    options.validate()?;
    stream(
        Cut {
            context_budget,
            store: None,
            header,
            through: 0,
            intervals: vec![],
            fork_sources: vec![],
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
    context_budget: rsi_agent_context::ContextBudget,
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
    let mut fork_sources = vec![];
    let mut leases = vec![lease];
    let inherited = rsi_agent_store_protocol::capture_inherited_intervals(store.as_ref(), &header)
        .await
        .map_err(|error| match error {
            rsi_agent_store_protocol::InheritedReadError::Store(error) => store_error(error),
            rsi_agent_store_protocol::InheritedReadError::Header(error) => encoding(error),
            other => invalid(&format!("export {other}")),
        })?;
    for row in inherited {
        let origin = row
            .source
            .history_origin()
            .ok_or_else(|| invalid("captured interval has no fork origin"))?;
        intervals.push(Interval {
            id: origin.parent_session_id.clone(),
            after: origin.resolved_after_seq,
            through: origin.resolved_terminal_seq,
        });
        fork_sources.push(row.source);
        leases.push(row.lease);
    }
    intervals.push(Interval {
        id: header.session_id().clone(),
        after: 0,
        through,
    });
    stream(
        Cut {
            context_budget,
            store: Some(store),
            header,
            through,
            intervals,
            fork_sources,
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
            let limit = usize::try_from((interval.through - after).min(FACT_PAGE_COUNT as u64)).expect("bounded page");
            let page = store.read_facts(&interval.id, after, limit, rsi_agent_store_protocol::MAXIMUM_STORE_FACT_PAGE_BYTES).await.map_err(store_error)?;
            if page.facts.is_empty() { Err(invalid("export history made no progress"))?; }
            for fact in page.facts {
                if fact.seq() != after + 1 || fact.seq() > interval.through { Err(invalid("export history escaped its cut"))?; }
                after = fact.seq();
                yield fact;
            }
        }
    })
}

#[allow(clippy::too_many_lines)] // One demand-driven stream owns section framing and backpressure.
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
                    let mut diagnostic = match &latest {
                        Some(latest) => diagnostic::request(&cut, latest).await?,
                        None => diagnostic::Diagnostic { value: json!({"availability":"unavailable","reason":"no_completed_conversation"}), credit: None },
                    };
                    if let Some(credit) = &mut diagnostic.credit {
                        let bytes = rendered_workspace_weight(&diagnostic.value, json_format)?;
                        let admitted = credit.bytes().checked_add(bytes);
                        if admitted.is_none_or(|bytes| credit.resize(bytes).is_err()) {
                            diagnostic.value = json!({"availability":"unavailable","reason":"context_capacity"});
                        }
                    }
                    yield render_value(&diagnostic.value, json_format)?;
                    drop(diagnostic);
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
                        let effect = (section == ExportInclude::LastProviderResponse).then(|| latest.as_ref().expect("selected attempt").effect.clone());
                        let mut records = projection::records(cut.store(), interval, section, reasoning, effect);
                        let mut markdown = projection::Markdown::default();
                        while let Some(record) = records.next().await {
                            let record = record?;
                            if json_format && !first { yield ",\n".into(); }
                            first = false;
                            yield if json_format { serde_json::to_string(&record).map_err(encoding)? } else { markdown.record(&record)? };
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
fn rendered_workspace_weight(value: &Value, json_format: bool) -> Result<usize> {
    #[derive(Default)]
    struct Counter {
        bytes: usize,
        ticks: usize,
        longest: usize,
    }
    impl std::io::Write for Counter {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.bytes = self
                .bytes
                .checked_add(bytes.len())
                .ok_or_else(|| std::io::Error::other("diagnostic size overflow"))?;
            for byte in bytes {
                if *byte == b'`' {
                    self.ticks += 1;
                    self.longest = self.longest.max(self.ticks);
                } else {
                    self.ticks = 0;
                }
            }
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut counter = Counter::default();
    serde_json::to_writer_pretty(&mut counter, value).map_err(encoding)?;
    if json_format {
        return Ok(counter.bytes);
    }
    // fenced() holds pretty JSON and the fence while building a complete output:
    // two pretty lengths, three fence lengths, "json" and four newlines.
    let fence = counter.longest.max(2).checked_add(1);
    counter
        .bytes
        .checked_mul(2)
        .and_then(|bytes| bytes.checked_add(fence?.checked_mul(3)?))
        .and_then(|bytes| bytes.checked_add(8))
        .ok_or_else(|| invalid("diagnostic size overflow"))
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
