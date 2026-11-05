use rsi_agent_session_protocol::{ModelAttemptLog, SessionFact, SessionFactBody};
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
struct Projection<'a> {
    seq: u64,
    retain_context: bool,
    events: Events<'a>,
}
struct Bounded(Vec<u8>);
impl Write for Bounded {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if self.0.len().saturating_add(bytes.len())
            > rsi_agent_session_protocol::MAXIMUM_SESSION_FACT_BYTES + 2
        {
            return Err(std::io::Error::other("attempt projection capacity"));
        }
        self.0.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
/// Encodes decoded, timestamped events from one exact authorized settlement Fact.
///
/// # Errors
/// Returns a protocol error for a different Fact or cursor, or a capacity
/// error when the decoded projection exceeds the Session Fact byte budget.
pub fn project_model_attempt(fact: &SessionFact, after: u64) -> Result<String> {
    let SessionFactBody::ModelAttemptSettled { attempt, .. } = fact.body() else {
        return Err(Error::Protocol);
    };
    if after.checked_add(1) != Some(fact.seq()) {
        return Err(Error::Protocol);
    }
    let mut bytes = Bounded(Vec::new());
    serde_json::to_writer(
        &mut bytes,
        &Projection {
            seq: fact.seq(),
            retain_context: attempt.retain_context(),
            events: Events(attempt),
        },
    )
    .map_err(|_| Error::Capacity)?;
    String::from_utf8(bytes.0).map_err(|_| Error::Protocol)
}
