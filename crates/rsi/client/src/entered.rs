//! Resolve final display rows at the owning Session boundary.
use rsi_agent_session_protocol::{PreparedInput, PreparedInputRole, SessionFact, SessionFactBody};
use rsi_conversation::{EnteredContent, EnteredInput};
use rsi_session_protocol::{SessionError, SessionHandle};

/// Visits the final user rows of a preparation in order. The sorted history page
/// can be empty for a live observation. Only a missing unchanged source needs I/O.
///
/// # Errors
/// Returns the Session read error or a missing or mismatched historical claim.
pub async fn visit_prepared_inputs(
    handle: &dyn SessionHandle,
    fact: &SessionFact,
    page: &[SessionFact],
    visit: impl FnMut(EnteredInput<'_>),
) -> Result<(), SessionError> {
    let lookup = |seq| {
        page.binary_search_by_key(&seq, SessionFact::seq)
            .ok()
            .map(|index| &page[index])
    };
    visit_inputs(handle, fact, lookup, visit).await
}

/// Small live batch of shared claim handles, released together after preparation.
#[derive(Debug, Default)]
pub struct LiveInputClaims {
    facts: std::collections::VecDeque<rsi_agent_turn_protocol::ObservedFact>,
    bytes: usize,
}

impl LiveInputClaims {
    /// Keeps recent claim sources for the next preparation without copying their bodies.
    pub fn observe(&mut self, fact: &rsi_agent_turn_protocol::ObservedFact) {
        match fact.body() {
            SessionFactBody::TurnTerminal { .. } => {
                self.facts.clear();
                self.bytes = 0;
            }
            SessionFactBody::TurnAccepted { .. }
            | SessionFactBody::InputMessageClaimed { .. }
            | SessionFactBody::ToolContextsClaimed { .. } => {
                let size = fact.encoded_len();
                if size > rsi_conversation::MAXIMUM_WINDOW_BYTES
                    || self
                        .facts
                        .back()
                        .is_some_and(|last| last.seq() >= fact.seq())
                {
                    return;
                }
                while (self.bytes + size > rsi_conversation::MAXIMUM_WINDOW_BYTES
                    || self.facts.len() >= rsi_agent_session_protocol::MAXIMUM_FACTS_PER_READ)
                    && let Some(oldest) = self.facts.pop_front()
                {
                    self.bytes -= oldest.encoded_len();
                }
                self.bytes += size;
                self.facts.push_back(fact.clone());
            }
            _ => {}
        }
    }

    /// Visits entered user rows, consulting history only for a missing claim.
    ///
    /// # Errors
    /// Returns the Session read error or a missing or mismatched historical claim.
    pub async fn prepare(
        self,
        handle: &dyn SessionHandle,
        fact: &SessionFact,
        visit: impl FnMut(EnteredInput<'_>),
    ) -> Result<(), SessionError> {
        let lookup = |seq| {
            self.facts
                .binary_search_by_key(&seq, |item| item.seq())
                .ok()
                .map(|i| self.facts[i].as_ref())
        };
        visit_inputs(handle, fact, lookup, visit).await
    }
}

async fn visit_inputs<'a>(
    handle: &dyn SessionHandle,
    fact: &SessionFact,
    lookup: impl Fn(u64) -> Option<&'a SessionFact>,
    mut visit: impl FnMut(EnteredInput<'_>),
) -> Result<(), SessionError> {
    let SessionFactBody::StepInputsPrepared {
        decision, turn_id, ..
    } = fact.body()
    else {
        return Ok(());
    };
    let mut last: Option<SessionFact> = None;
    let mut authored = 0;
    // A Tool claim may contain thousands of messages in any requested order.
    let mut tool_offsets = (0, Vec::new());
    for (index, entry) in decision.messages().unwrap_or_default().iter().enumerate() {
        let row = match entry {
            PreparedInput::Added { message } | PreparedInput::Replacement { message, .. } => {
                let offset = authored;
                authored += u32::try_from(message.content().len()).expect("bounded prepared input");
                if message.role() == PreparedInputRole::Developer {
                    continue;
                }
                EnteredInput {
                    index,
                    source: None,
                    seq: fact.seq(),
                    offset,
                    content: EnteredContent::Blocks(message.content()),
                }
            }
            PreparedInput::Claimed { claim } => {
                let seq = claim.fact_seq(fact.seq());
                let invalid = || {
                    SessionError::Invalid(
                        "Prepared display input is outside its source claim".into(),
                    )
                };
                let original = if let Some(found) = lookup(seq) {
                    found
                } else {
                    if last.as_ref().is_none_or(|source| source.seq() != seq) {
                        last = handle
                            .history_before(Some(seq.checked_add(1).ok_or_else(invalid)?), 1)
                            .await?
                            .facts
                            .pop();
                    }
                    last.as_ref()
                        .filter(|f| f.seq() == seq)
                        .ok_or_else(invalid)?
                };
                if original.body().turn_id() != turn_id {
                    return Err(invalid());
                }
                let (source, offset, content) = match (original.body(), claim.ordinal()) {
                    (SessionFactBody::TurnAccepted { text, .. }, 0) => {
                        (None, 0, EnteredContent::Direct(text))
                    }
                    (
                        SessionFactBody::InputMessageClaimed {
                            source, content, ..
                        },
                        0,
                    ) => {
                        if source.input_role() == PreparedInputRole::Developer {
                            continue;
                        }
                        (Some(source), 0, EnteredContent::Blocks(content))
                    }
                    (SessionFactBody::ToolContextsClaimed { messages, .. }, ordinal) => {
                        let pos = ordinal as usize;
                        let message = &messages.get(pos).ok_or_else(invalid)?.message;
                        if tool_offsets.0 != seq {
                            tool_offsets.0 = seq;
                            tool_offsets.1.clear();
                            let mut offset = 0;
                            for row in messages {
                                tool_offsets.1.push(offset);
                                offset += u32::try_from(row.message.text.len())
                                    .expect("bounded tool context");
                            }
                        }
                        (
                            None,
                            tool_offsets.1[pos],
                            EnteredContent::Tool(&message.text),
                        )
                    }
                    _ => return Err(invalid()),
                };
                EnteredInput {
                    index,
                    source,
                    seq,
                    offset,
                    content,
                }
            }
        };
        visit(row);
    }
    Ok(())
}
