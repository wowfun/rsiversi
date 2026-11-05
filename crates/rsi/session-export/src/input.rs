//! Resolve conversation inputs at their committed preparation cut.
use super::{Result, SessionFact, SessionId, SessionStore, Value, invalid, json, store_error};
use rsi_agent_session_protocol::{
    InputClaimRef, PreparedInput, PreparedInputMessage, PreparedInputRole, SessionFactBody,
};
use rsi_agent_store_protocol::MAXIMUM_STORE_FACT_PAGE_BYTES;
use std::collections::VecDeque;

pub(super) struct Claims {
    after: u64,
    bytes: usize,
    recent: VecDeque<SessionFact>,
    reread: Option<SessionFact>,
}

impl Claims {
    pub(super) fn new(after: u64) -> Self {
        Self {
            after,
            bytes: 0,
            recent: VecDeque::new(),
            reread: None,
        }
    }

    pub(super) fn observe(&mut self, fact: SessionFact) {
        while self.recent.len() >= super::FACT_PAGE_COUNT
            || self.bytes + fact.encoded_len() > MAXIMUM_STORE_FACT_PAGE_BYTES
        {
            let Some(oldest) = self.recent.pop_front() else {
                break;
            };
            self.bytes -= oldest.encoded_len();
        }
        self.bytes += fact.encoded_len();
        self.recent.push_back(fact);
    }

    async fn original(
        &mut self,
        store: &dyn SessionStore,
        id: &SessionId,
        prepared: &SessionFact,
        claim: InputClaimRef,
    ) -> Result<&SessionFact> {
        let seq = claim.fact_seq(prepared.seq());
        if seq <= self.after || seq >= prepared.seq() {
            return Err(invalid(
                "export input claim escaped its preparation interval",
            ));
        }
        let fact = if let Ok(index) = self.recent.binary_search_by_key(&seq, SessionFact::seq) {
            &self.recent[index]
        } else {
            if self.reread.as_ref().is_none_or(|fact| fact.seq() != seq) {
                self.reread = store
                    .read_facts(id, seq - 1, 1, MAXIMUM_STORE_FACT_PAGE_BYTES)
                    .await
                    .map_err(store_error)?
                    .facts
                    .into_iter()
                    .next();
            }
            self.reread
                .as_ref()
                .ok_or_else(|| invalid("missing export input claim"))?
        };
        if fact.seq() != seq
            || fact.body().turn_id() != prepared.body().turn_id()
            || claim.ordinal() as usize >= fact.claimed_input_count()
        {
            return Err(invalid("export input claim does not match its source"));
        }
        Ok(fact)
    }

    pub(super) async fn record(
        &mut self,
        store: &dyn SessionStore,
        id: &SessionId,
        fact: &SessionFact,
        input: &PreparedInput,
        index: usize,
        offset: &mut usize,
    ) -> Result<Option<Value>> {
        let (claim, row) = match input {
            PreparedInput::Claimed { claim } => {
                let source = self.original(store, id, fact, *claim).await?;
                (Some(*claim), original_row(source, *claim)?)
            }
            PreparedInput::Replacement { claim, message } => {
                self.original(store, id, fact, *claim).await?;
                (Some(*claim), authored_row(message, offset))
            }
            PreparedInput::Added { message } => (None, authored_row(message, offset)),
        };
        let Some(mut row) = row else { return Ok(None) };
        if let Some(claim) = claim {
            row["claim"] = json!({"fact_seq":claim.fact_seq(fact.seq()),"ordinal":claim.ordinal()});
        }
        row["session_id"] = json!(id);
        row["turn_id"] = json!(fact.body().turn_id());
        row["seq"] = json!(fact.seq().to_string());
        row["input_index"] = json!(index);
        Ok(Some(row))
    }
}

fn authored_row(message: &PreparedInputMessage, offset: &mut usize) -> Option<Value> {
    let start = *offset;
    *offset += message.content().len();
    (message.role() == PreparedInputRole::User).then(|| json!({
        "role":"user", "message_id":message.attribution().id, "source":message.attribution().source,
        "content":message.content(), "content_offset":start
    }))
}

fn original_row(fact: &SessionFact, claim: InputClaimRef) -> Result<Option<Value>> {
    let row = match fact.body() {
        SessionFactBody::TurnAccepted { text, .. } => json!({"role":"user","text":text}),
        SessionFactBody::InputMessageClaimed {
            source, content, ..
        } => {
            if source.input_role() == PreparedInputRole::Developer {
                return Ok(None);
            }
            json!({"role":"user","source":source,"content":content})
        }
        SessionFactBody::ToolContextsClaimed { messages, .. } => {
            let message = &messages[claim.ordinal() as usize].message;
            let mut row = json!({"role":"user","message_id":message.id,"source":message.source});
            row["content"] = Value::Array(
                message
                    .text
                    .iter()
                    .map(|text| json!({"type":"text","text":text}))
                    .collect(),
            );
            row
        }
        _ => return Err(invalid("unexpected export input source")),
    };
    Ok(Some(row))
}
