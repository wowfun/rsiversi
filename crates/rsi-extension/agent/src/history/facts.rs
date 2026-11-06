use rsi_agent_session_protocol::{
    EffectId, ModelAttemptLog, ModelEventPurpose, SessionFact, SessionFactBody, TurnId,
};
use rsi_extension::{Error, Result};
use serde::{Serialize, ser::SerializeSeq};
use std::io::Write;

struct Events<'a>(&'a ModelAttemptLog);
impl Serialize for Events<'_> {
    fn serialize<S: serde::Serializer>(
        &self,
        serializer: S,
    ) -> std::result::Result<S::Ok, S::Error> {
        let mut sequence = serializer.serialize_seq(None)?;
        for event in self.0.events() {
            sequence.serialize_element(&event.map_err(serde::ser::Error::custom)?)?;
        }
        sequence.end()
    }
}

#[derive(Serialize)]
struct Attempt<'a> {
    retain_context: bool,
    events: Events<'a>,
}
#[derive(Serialize)]
struct AttemptFact<'a> {
    seq: u64,
    timestamp_ms: u64,
    #[serde(rename = "type")]
    kind: &'static str,
    turn_id: &'a TurnId,
    effect_id: &'a EffectId,
    purpose: &'a ModelEventPurpose,
    attempt: Attempt<'a>,
}
struct Fact<'a>(&'a SessionFact);
impl Serialize for Fact<'_> {
    fn serialize<S: serde::Serializer>(
        &self,
        serializer: S,
    ) -> std::result::Result<S::Ok, S::Error> {
        match self.0.body() {
            SessionFactBody::ModelAttemptSettled {
                turn_id,
                effect_id,
                purpose,
                attempt,
            } => AttemptFact {
                seq: self.0.seq(),
                timestamp_ms: self.0.timestamp_ms(),
                kind: "model_attempt_settled",
                turn_id,
                effect_id,
                purpose,
                attempt: Attempt {
                    retain_context: attempt.retain_context(),
                    events: Events(attempt),
                },
            }
            .serialize(serializer),
            _ => self.0.serialize(serializer),
        }
    }
}

struct Bounded {
    bytes: Vec<u8>,
    maximum: usize,
}
impl Write for Bounded {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if self.bytes.len().saturating_add(bytes.len()) > self.maximum {
            return Err(std::io::Error::other("history page capacity"));
        }
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Encodes a captured Fact page with model events decoded in place of durable chunks.
/// Returns the covered cursor and its bounded JSON array. Claim-invisible positions
/// remain covered by `through`; a page ending early stops before the next visible Fact.
///
/// # Errors
/// Returns capacity for an individually oversized projection, or protocol for an
/// invalid canonical event. No partial Fact is returned.
pub fn project_history_page<'a>(
    facts: impl IntoIterator<Item = &'a SessionFact>,
    through: u64,
) -> Result<(u64, String)> {
    encode_page(
        facts,
        through,
        rsi_agent_session_protocol::MAXIMUM_SESSION_FACT_BYTES + 2,
    )
}

fn encode_page<'a>(
    facts: impl IntoIterator<Item = &'a SessionFact>,
    mut through: u64,
    maximum: usize,
) -> Result<(u64, String)> {
    let mut output = Bounded {
        bytes: vec![b'['],
        maximum: maximum - 1,
    };
    for fact in facts {
        if fact.seq() > through {
            break;
        }
        let start = output.bytes.len();
        if start > 1 {
            output.bytes.push(b',');
        }
        if let Err(error) = serde_json::to_writer(&mut output, &Fact(fact)) {
            if !error.is_io() {
                return Err(Error::Protocol);
            }
            if start == 1 {
                return Err(Error::Capacity);
            }
            output.bytes.truncate(start);
            through = fact.seq() - 1;
            break;
        }
    }
    output.bytes.push(b']');
    Ok((
        through,
        String::from_utf8(output.bytes).map_err(|_| Error::Protocol)?,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn page_capacity_keeps_whole_facts_and_claim_invisible_cursors() {
        let fact = |seq| {
            SessionFact::new(
                seq,
                1,
                SessionFactBody::CancelRequested {
                    turn_id: TurnId::new("turn").unwrap(),
                    reason: None,
                },
            )
            .unwrap()
        };
        let facts = [fact(2), fact(5)];
        let expected = serde_json::to_string(&facts[..1]).unwrap();
        let (through, json) = encode_page(&facts, 7, expected.len()).unwrap();
        assert_eq!(through, 4);
        assert_eq!(json, expected);
        assert!(matches!(
            encode_page(&facts, 7, expected.len() - 1),
            Err(Error::Capacity)
        ));
        assert_eq!(encode_page([], 7, 2).unwrap(), (7, "[]".into()));
        let (through, json) = project_history_page(&facts, 7).unwrap();
        assert_eq!(through, 7);
        assert_eq!(json, serde_json::to_string(&facts).unwrap());
    }
}
