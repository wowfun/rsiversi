use super::{Result, SessionFact, SessionId, Value, encoding, json, render_value};
use rsi_agent_session_protocol::{EffectId, SessionFactBody};
use rsi_ai_protocol::{ContentDelta, ContentStart, LanguageEvent};
use std::collections::BTreeSet;

pub(super) fn records(
    store: std::sync::Arc<dyn super::SessionStore>,
    interval: super::Interval,
    section: super::ExportInclude,
    reasoning: bool,
    effect: Option<EffectId>,
) -> std::pin::Pin<Box<dyn futures_util::Stream<Item = Result<Value>> + Send>> {
    use futures_util::StreamExt;
    Box::pin(async_stream::try_stream! {
        let id = interval.id.clone();
        let mut claims = super::input::Claims::new(interval.after);
        let mut source = super::facts(store.clone(), interval);
        let mut projection = Projection::default();
        while let Some(fact) = source.next().await {
            let fact = fact?;
            if section == super::ExportInclude::ProviderInputEvidence {
                if let Some(record) = super::diagnostic::evidence_record(&*store, &id, &fact).await? { yield record; }
                continue;
            }
            match fact.body() {
                SessionFactBody::ModelAttemptSettled { effect_id, attempt, purpose: rsi_agent_session_protocol::ModelEventPurpose::Conversation, .. } => {
                    if effect.as_ref().is_some_and(|wanted|wanted != effect_id) { continue; }
                    for (index, item) in attempt.events().enumerate() {
                        let (elapsed, event) = item.map_err(encoding)?;
                        if let Some(row) = projection.event(&id, &fact, effect_id, &event, reasoning, (index, elapsed))? { yield row; }
                    }
                }
                _ if effect.is_some() => {}
                _ if fact.claimed_input_count() > 0 => claims.observe(fact),
                SessionFactBody::StepInputsPrepared { decision, .. } => {
                    let mut offset = 0;
                    for (index, input) in decision.messages().unwrap_or_default().iter().enumerate() {
                        if let Some(row) = claims.record(&*store, &id, &fact, input, index, &mut offset).await? { yield row; }
                    }
                    claims = super::input::Claims::new(fact.seq());
                }
                SessionFactBody::TurnTerminal { .. } => claims = super::input::Claims::new(fact.seq()),
                _ => if let Some(row) = projection.record(&id, &fact)? { yield row; },
            }
        }
    })
}

#[derive(Default)]
pub(super) struct Projection {
    reasoning: BTreeSet<u32>,
}
impl Projection {
    pub(super) fn event(
        &mut self,
        id: &SessionId,
        fact: &SessionFact,
        effect_id: &EffectId,
        event: &LanguageEvent,
        reasoning: bool,
        position: (usize, u64),
    ) -> Result<Option<Value>> {
        let (event_index, elapsed_ms) = position;
        let event = match event {
            LanguageEvent::ContentStarted {
                index,
                content: ContentStart::Reasoning,
            } => {
                self.reasoning.insert(*index);
                if !reasoning {
                    return Ok(None);
                }
                serde_json::to_value(event).map_err(encoding)?
            }
            LanguageEvent::ContentDelta {
                delta: ContentDelta::Reasoning(_),
                ..
            } if !reasoning => return Ok(None),
            LanguageEvent::ContentFinished { index }
                if self.reasoning.remove(index) && !reasoning =>
            {
                return Ok(None);
            }
            LanguageEvent::Finished { reason, .. } => {
                json!({"type":"finished","reason":reason})
            }
            LanguageEvent::Failed { error, .. } => json!({"type":"failed","error":error}),
            _ => serde_json::to_value(event).map_err(encoding)?,
        };
        Ok(Some(
            json!({"session_id":id,"seq":fact.seq().to_string(),"turn_id":fact.body().turn_id(),
            "role":"assistant","effect_id":effect_id,"event_index":event_index,"elapsed_ms":elapsed_ms,
            "partial":!matches!(event["type"].as_str(),Some("finished"|"failed")),"event":event}),
        ))
    }

    pub(super) fn record(&mut self, id: &SessionId, fact: &SessionFact) -> Result<Option<Value>> {
        let body = fact.body();
        if let SessionFactBody::ModelIntent { .. } = body {
            self.reasoning.clear();
        }
        let value = match body {
            SessionFactBody::ImageRequested { request, .. } => {
                json!({"role":"user","text":request.prompt()})
            }
            SessionFactBody::ToolIntent {
                name,
                identity,
                arguments,
                origin,
                program_role,
                ..
            } => {
                json!({"role":"tool_call","name":name,"identity":identity,"arguments":arguments,"origin":origin,"program_role":program_role})
            }
            SessionFactBody::ToolResult {
                identity, result, ..
            } => {
                json!({"role":"tool","identity":identity,"content":result.content,"value":result.value,"is_error":result.is_error})
            }
            SessionFactBody::ToolRejected { identity, .. } => {
                let mut value = serde_json::to_value(body).map_err(encoding)?;
                value["role"] = json!("tool_rejected");
                value["identity"] = json!(identity);
                value
            }
            SessionFactBody::ImageOutput { media, .. } => json!({"role":"assistant","media":media}),
            _ => return Ok(None),
        };
        let mut value = value;
        value["session_id"] = json!(id);
        value["seq"] = json!(fact.seq().to_string());
        value["turn_id"] = json!(body.turn_id());
        Ok(Some(value))
    }
}

#[derive(Default)]
pub(super) struct Markdown {
    block: Option<(String, String, u64)>,
    partial: bool,
}
impl Markdown {
    pub(super) fn record(&mut self, record: &Value) -> Result<String> {
        let role = record["role"].as_str().unwrap_or("evidence");
        let heading = || {
            format!(
                "### {} · {}:{}\n\n",
                role,
                record["session_id"].as_str().unwrap_or(""),
                record["seq"].as_str().unwrap_or("")
            )
        };
        let event = &record["event"];
        if role == "assistant" && !event.is_null() {
            self.partial = !matches!(event["type"].as_str(), Some("finished" | "failed"));
            let key = (
                record["session_id"].as_str().unwrap_or("").to_owned(),
                record["effect_id"].as_str().unwrap_or("").to_owned(),
                event["index"].as_u64().unwrap_or(0),
            );
            if event["type"] == "content_started"
                && matches!(
                    event["content"]["type"].as_str(),
                    Some("text" | "reasoning")
                )
            {
                self.block = Some(key);
                let mut text = format!("\n{}", heading());
                if event["content"]["type"] == "reasoning" {
                    text.push_str("Reasoning:\n\n");
                }
                return Ok(text);
            }
            if event["type"] == "content_delta"
                && matches!(event["delta"]["type"].as_str(), Some("text" | "reasoning"))
            {
                let mut text = String::new();
                if self.block.as_ref() != Some(&key) {
                    text.push('\n');
                    text.push_str(&heading());
                    if event["delta"]["type"] == "reasoning" {
                        text.push_str("Reasoning:\n\n");
                    }
                    self.block = Some(key);
                }
                text.push_str(event["delta"]["value"].as_str().unwrap_or(""));
                return Ok(text);
            }
            if event["type"] == "content_finished" && self.block.as_ref() == Some(&key) {
                self.block = None;
                return Ok("\n\n".into());
            }
        }
        let mut out = format!("\n{}", heading());
        self.block = None;
        if let Some(text) = record["text"].as_str() {
            out.push_str(text);
            out.push_str("\n\n");
        } else {
            out.push_str(&render_value(record, false)?);
        }
        Ok(out)
    }
    pub(super) fn finish(&self) -> &'static str {
        if self.partial {
            "\n\n_Partial generation: no completion is present in the captured history._\n"
        } else {
            "\n"
        }
    }
}
