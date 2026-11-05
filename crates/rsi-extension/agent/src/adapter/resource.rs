use super::{AgentAdapter, CancellationToken, ContributionResult, async_trait, invalid};
use rsi_agent_composition_protocol::SessionResourceReader;
use rsi_agent_session_protocol::{SessionHeader, SessionResourceDescriptor, SessionResourceValue};

fn descriptor(id: &str) -> SessionResourceDescriptor {
    SessionResourceDescriptor {
        id: id.into(),
        name: "Recorded extension events".into(),
        description: "Canonical extension journal data".into(),
        source: "rsi.extension.journal".into(),
        media_type: "application/json".into(),
        model_readable: false,
    }
}

#[async_trait]
impl SessionResourceReader for AgentAdapter {
    async fn read(
        &self,
        header: &SessionHeader,
        _execution: Option<&rsi_execution::ExecutionLease>,
        id: Option<&str>,
        cancellation: CancellationToken,
    ) -> ContributionResult<SessionResourceValue> {
        if cancellation.is_cancelled() {
            return Err(super::ContributionError::Closed);
        }
        let Some(id) = id else {
            return Ok(SessionResourceValue::List {
                entries: vec![descriptor("head")],
            });
        };
        if id != "head"
            && (id.len() != 64
                || !id
                    .bytes()
                    .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()))
        {
            return Err(invalid("invalid journal resource coordinate"));
        }
        let page = self
            .store
            .read_domain_states(
                header.session_id(),
                None,
                Some(rsi_agent_session_protocol::EXTENSION_JOURNAL_DOMAIN),
            )
            .await
            .map_err(invalid)?;
        let view = page
            .states
            .into_iter()
            .next()
            .ok_or_else(|| invalid("Session journal is unavailable"))?;
        let journal = self.handle.decode(&view.snapshot).map_err(invalid)?;
        if journal.artifact_sha256 != self.artifact_sha256 {
            return Err(invalid("journal artifact cohort mismatch"));
        }
        let text = if id == "head" {
            serde_json::to_string(&serde_json::json!({"revision":view.head.revision,"artifact_sha256":journal.artifact_sha256,"segments":journal.segments,"checkpoint":journal.checkpoint})).map_err(invalid)?
        } else {
            let segment = journal
                .segments
                .iter()
                .find(|segment| segment.sha256 == id)
                .ok_or_else(|| {
                    invalid("journal segment is no longer in the captured Session; reload its head")
                })?;
            let events = crate::restore_segment(
                self.store.as_ref(),
                segment,
                self.read_budget.clone().into(),
            )
            .await
            .map_err(invalid)?;
            serde_json::to_string(&events).map_err(invalid)?
        };
        if cancellation.is_cancelled() {
            return Err(super::ContributionError::Closed);
        }
        Ok(SessionResourceValue::Read {
            resource: descriptor(id),
            text,
        })
    }
}
