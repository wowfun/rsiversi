use crate::encoding::BoundedBytes;
use rsi_agent_session_protocol::{
    DomainRequestId, EffectId, ManualCompactionEvent, ModelAttemptLog, ModelEventPurpose,
    SessionFact, SessionFactBody, TurnId,
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
impl<'a> From<&'a ModelAttemptLog> for Attempt<'a> {
    fn from(attempt: &'a ModelAttemptLog) -> Self {
        Self {
            retain_context: attempt.retain_context(),
            events: Events(attempt),
        }
    }
}
#[derive(Serialize)]
struct AttemptFact<'a> {
    seq: u64,
    timestamp_ms: u64,
    #[serde(flatten)]
    body: Settlement<'a>,
}
#[derive(Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum Settlement<'a> {
    ModelAttemptSettled {
        turn_id: &'a TurnId,
        effect_id: &'a EffectId,
        purpose: &'a ModelEventPurpose,
        attempt: Attempt<'a>,
    },
    ManualCompaction {
        command_id: &'a DomainRequestId,
        effect_id: &'a EffectId,
        event: CompactionSettlement<'a>,
    },
}
#[derive(Serialize)]
struct CompactionSettlement<'a> {
    stage: &'static str,
    attempt: Attempt<'a>,
}
struct Fact<'a>(&'a SessionFact);
impl Serialize for Fact<'_> {
    fn serialize<S: serde::Serializer>(
        &self,
        serializer: S,
    ) -> std::result::Result<S::Ok, S::Error> {
        let body = match self.0.body() {
            SessionFactBody::ModelAttemptSettled {
                turn_id,
                effect_id,
                purpose,
                attempt,
            } => Settlement::ModelAttemptSettled {
                turn_id,
                effect_id,
                purpose,
                attempt: attempt.into(),
            },
            SessionFactBody::ManualCompaction {
                command_id,
                effect_id,
                event: ManualCompactionEvent::Settled { attempt },
            } => Settlement::ManualCompaction {
                command_id,
                effect_id,
                event: CompactionSettlement {
                    stage: "settled",
                    attempt: attempt.into(),
                },
            },
            _ => return self.0.serialize(serializer),
        };
        AttemptFact {
            seq: self.0.seq(),
            timestamp_ms: self.0.timestamp_ms(),
            body,
        }
        .serialize(serializer)
    }
}

/// Encodes a captured Fact page with model events decoded in place of durable chunks.
/// Returns the covered cursor and its bounded JSON array. Claim-invisible positions
/// remain covered by `through`; a page ending early stops before the next visible Fact.
///
/// # Errors
/// Returns capacity for an individually oversized projection, or protocol for an
/// invalid canonical event or a budget outside the serialized Fact ceiling.
/// No partial Fact is returned.
pub fn project_history_page<'a>(
    facts: impl IntoIterator<Item = &'a SessionFact>,
    mut through: u64,
    maximum: usize,
) -> Result<(u64, String)> {
    if !(2..=rsi_agent_session_protocol::MAXIMUM_SESSION_FACT_BYTES + 2).contains(&maximum) {
        return Err(Error::Protocol);
    }
    let mut output = BoundedBytes {
        bytes: vec![b'['],
        maximum: maximum - 1,
    };
    for fact in facts {
        if fact.seq() > through {
            break;
        }
        let start = output.bytes.len();
        if start > 1 && output.write_all(b",").is_err() {
            through = fact.seq() - 1;
            break;
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
    output.bytes.reserve_exact(1);
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
    fn manual_settlement_keeps_its_command_and_decodes_the_provider_attempt() {
        let events = serde_json::json!([
            [0, {"type":"content_started","index":0,"content":{"type":"text"}}],
            [7, {"type":"content_delta","index":0,"delta":{"type":"text","value":"checkpoint"}}],
            [12, {"type":"content_finished","index":0}],
            [19, {"type":"finished","reason":"stop","replay":null}]
        ]);
        let attempt = ModelAttemptLog::from_events(
            serde_json::from_value::<Vec<_>>(events.clone()).unwrap(),
            false,
        )
        .unwrap();
        let fact = SessionFact::new(
            7,
            21,
            SessionFactBody::ManualCompaction {
                command_id: DomainRequestId::new("manual").unwrap(),
                effect_id: EffectId::new("summary").unwrap(),
                event: ManualCompactionEvent::Settled { attempt },
            },
        )
        .unwrap();
        let expected = serde_json::json!([{"seq":7,"timestamp_ms":21,"type":"manual_compaction",
            "command_id":"manual","effect_id":"summary","event":{"stage":"settled",
            "attempt":{"retain_context":false,"events":events}}}]);
        let (through, page) = project_history_page([&fact], 7, 1024).unwrap();
        assert_eq!(through, 7);
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&page).unwrap(),
            expected
        );
        assert!(matches!(
            project_history_page([&fact], 7, page.len() - 1),
            Err(Error::Capacity)
        ));
    }

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
        let (through, json) = project_history_page(&facts, 7, expected.len()).unwrap();
        assert_eq!(through, 4);
        assert_eq!(json, expected);
        assert!(matches!(
            project_history_page(&facts, 7, expected.len() - 1),
            Err(Error::Capacity)
        ));
        assert_eq!(project_history_page([], 7, 2).unwrap(), (7, "[]".into()));
        for maximum in [
            0,
            1,
            rsi_agent_session_protocol::MAXIMUM_SESSION_FACT_BYTES + 3,
        ] {
            assert!(matches!(
                project_history_page([], 7, maximum),
                Err(Error::Protocol)
            ));
        }
        let (through, json) = project_history_page(
            &facts,
            7,
            rsi_agent_session_protocol::MAXIMUM_SESSION_FACT_BYTES + 2,
        )
        .unwrap();
        assert_eq!(through, 7);
        assert_eq!(json, serde_json::to_string(&facts).unwrap());
    }
}
