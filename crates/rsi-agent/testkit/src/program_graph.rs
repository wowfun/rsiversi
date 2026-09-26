use super::{
    AgentControlRecord, MemoryState, MessageId, Result, SessionHeader, SessionId,
    StoreAgentMessage, StoreError,
};
use rsi_agent_store_protocol::{ProgramGraphQuery, ProgramGraphRead};
pub(super) struct Graph<'a>(pub &'a MemoryState);
impl ProgramGraphRead for Graph<'_> {
    fn header(&self, session: &SessionId) -> Result<SessionHeader> {
        self.0
            .sessions
            .get(session)
            .map(|session| session.header.clone())
            .ok_or_else(missing)
    }
    fn message(&self, session: &SessionId, message: &MessageId) -> Result<StoreAgentMessage> {
        self.0
            .agent_messages
            .get(&(session.clone(), message.clone()))
            .cloned()
            .ok_or_else(missing)
    }
    fn control(
        &self,
        session: &SessionId,
        query: ProgramGraphQuery<'_>,
    ) -> Result<AgentControlRecord> {
        let session = self.0.sessions.get(session).ok_or_else(missing)?;
        let mut found = session
            .controls
            .iter()
            .filter(|record| query.matches(record.body()));
        let record = found.next().ok_or_else(missing)?;
        if found.next().is_some() {
            return Err(missing());
        }
        Ok(record.clone())
    }
}
fn missing() -> StoreError {
    StoreError::Invalid("Program graph counterpart is absent, duplicated or inconsistent".into())
}
