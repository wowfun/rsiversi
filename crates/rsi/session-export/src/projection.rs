use super::{Result, SessionFact, SessionId, Value, encoding, json, render_value};
use rsi_agent_session_protocol::{
    EffectId, InputMessageSource, ModelEventPurpose, SessionFactBody,
};
use rsi_ai_protocol::{ContentDelta, ContentStart, LanguageEvent};
use std::collections::BTreeSet;

#[derive(Default)]
pub(super) struct Projection {
    reasoning: BTreeSet<u32>,
}
impl Projection {
    pub(super) fn record(
        &mut self,
        id: &SessionId,
        fact: &SessionFact,
        reasoning: bool,
        effect: Option<&EffectId>,
    ) -> Result<Option<Value>> {
        let body = fact.body();
        if let SessionFactBody::ModelIntent { .. } = body {
            self.reasoning.clear();
        }
        let value = match body {
            SessionFactBody::ModelEvent {
                effect_id,
                event,
                purpose: ModelEventPurpose::Conversation,
                ..
            } if effect.is_none_or(|selected| selected == effect_id) => {
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
                json!({"role":"assistant","effect_id":effect_id,"partial": !matches!(event["type"].as_str(),Some("finished"|"failed")),"event":event})
            }
            _ if effect.is_some() => return Ok(None),
            SessionFactBody::TurnAccepted { text, .. } => json!({"role":"user","text":text}),
            SessionFactBody::ImageRequested { request, .. } => {
                json!({"role":"user","text":request.prompt()})
            }
            SessionFactBody::InputMessageEntered {
                source, content, ..
            } if matches!(
                source,
                InputMessageSource::Human { .. }
                    | InputMessageSource::Agent { .. }
                    | InputMessageSource::Completion { .. }
                    | InputMessageSource::Continuation { .. }
            ) =>
            {
                json!({"role":"user","source":source,"content":content})
            }
            SessionFactBody::ToolIntent {
                name,
                identity,
                arguments,
                ..
            } => json!({"role":"tool_call","name":name,"identity":identity,"arguments":arguments}),
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
