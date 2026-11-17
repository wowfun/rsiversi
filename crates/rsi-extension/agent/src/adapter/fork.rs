use super::{
    AgentAdapter, CancellationToken, ContributionError, ContributionResult, Deserialize,
    DomainStateValue, ExtensionJournal, Ordering, async_trait, invalid, json, prepare, read_error,
};
use rsi_agent_composition_protocol::{
    ForkTransform, ForkTransformContext, ValidatedDomainProposal,
};
use rsi_agent_session_protocol::DomainRevision;
use std::sync::atomic::AtomicU64;

static NEXT_OPERATION: AtomicU64 = AtomicU64::new(1);

fn transport_error(error: rsi_extension::RequestFailure) -> ContributionError {
    match error {
        rsi_extension::RequestFailure::NotDispatched(cause)
        | rsi_extension::RequestFailure::MayHaveExecuted(cause) => read_error(cause),
    }
}
fn journal_error(error: crate::Error) -> ContributionError {
    match error {
        crate::Error::Capacity => ContributionError::Capacity,
        other @ crate::Error::Invalid(_) => invalid(other),
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ResultHeader {
    count: u64,
    checkpoint: DomainStateValue,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Page {
    offset: u64,
    next_offset: u64,
    events: rsi_extension::body::Body,
}

#[async_trait]
impl ForkTransform for AgentAdapter {
    async fn transform(
        &self,
        context: &ForkTransformContext,
        cancellation: CancellationToken,
    ) -> ContributionResult<Vec<ValidatedDomainProposal>> {
        let Some(snapshot) = context.domains.iter().find(|snapshot| {
            snapshot.identity().id() == rsi_agent_session_protocol::EXTENSION_JOURNAL_DOMAIN
        }) else {
            return Ok(vec![]);
        };
        let current = self.handle.decode(snapshot).map_err(invalid)?;
        let serial = NEXT_OPERATION
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |value| {
                value.checked_add(1)
            })
            .map_err(|_| ContributionError::Capacity)?;
        let operation = format!("fork-{serial}");
        let _history = self
            .history
            .enter_reader(
                context.source.session_id().as_str(),
                &operation,
                context.facts.clone(),
                context.through_seq,
                &cancellation,
            )
            .map_err(read_error)?;
        let result = self
            .transform_journal(context, &current, &operation, cancellation)
            .await;
        // This is disposable pure work. Lost release has no publication ambiguity;
        // the peer's finite transfer lease also covers cancellation of this owner.
        let _released = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            self.connection.request(
                "agent.fork.release",
                json!({"operation_id": operation}),
                CancellationToken::new(),
            ),
        )
        .await;
        result.map(|proposal| vec![proposal])
    }
}

impl AgentAdapter {
    #[allow(clippy::too_many_lines)] // One paged transaction retains input and output CAS owners through the final proposal.
    async fn transform_journal(
        &self,
        context: &ForkTransformContext,
        current: &ExtensionJournal,
        operation: &str,
        cancellation: CancellationToken,
    ) -> ContributionResult<ValidatedDomainProposal> {
        self.connection.request("agent.fork.begin", json!({
            "operation_id": operation, "session_id": context.source.session_id(), "created_at": context.source.created_at_ms(),
            "count": current.segments.iter().map(|segment| segment.count).sum::<u64>(), "checkpoint": current.checkpoint,
            "after_seq": context.after_seq, "through_seq": context.through_seq,
            "include_inherited": matches!(context.selection, rsi_agent_session_protocol::ForkTurnSelection::All),
        }), cancellation.clone()).await.map_err(transport_error)?;
        for segment in &current.segments {
            let events = crate::restore_segment_bytes(
                self.store.as_ref(),
                segment,
                self.read_budget.clone().into(),
            )
            .await
            .map_err(journal_error)?;
            let (body, _body_scope) = self
                .arguments
                .enter_bytes(
                    context.source.session_id().as_str(),
                    operation,
                    events,
                    &cancellation,
                )
                .map_err(read_error)?;
            self.connection
                .request(
                    "agent.fork.events",
                    json!({"session_id":context.source.session_id(),"operation_id": operation, "offset": segment.first, "events": body}),
                    cancellation.clone(),
                )
                .await
                .map_err(transport_error)?;
        }
        let reply: ResultHeader = serde_json::from_value(
            self.connection
                .request(
                    "agent.fork.finish",
                    json!({"operation_id": operation}),
                    cancellation.clone(),
                )
                .await
                .map_err(transport_error)?,
        )
        .map_err(invalid)?;
        if reply.count > 100_000 || reply.checkpoint.encoded_len() > 64 * 1024 {
            return Err(ContributionError::Capacity);
        }
        let mut roots = current.sessions.clone();
        if !roots.contains(context.source.session_id()) {
            roots.push(context.source.session_id().clone());
        }
        let mut journal = ExtensionJournal {
            segments: vec![],
            sessions: roots,
            checkpoint: reply.checkpoint.clone(),
        };
        journal.validate().map_err(invalid)?;
        let mut prepared = prepare(
            self.store.as_ref(),
            &self.handle,
            DomainRevision::default(),
            &journal,
            crate::events::Events::default(),
            reply.checkpoint.clone(),
            self.read_budget.clone().into(),
        )
        .await
        .map_err(journal_error)?;
        let mut cursor = 0;
        let reads = super::bodies::Reads::new(self, &cancellation);
        while cursor < reply.count {
            let page: Page = serde_json::from_value(
                reads
                    .request(
                        super::bodies::ReadMethod::ForkPage,
                        json!({"operation_id": operation, "offset": cursor}),
                    )
                    .await
                    .map_err(read_error)?,
            )
            .map_err(invalid)?;
            let events = reads
                .events(
                    page.events,
                    super::bodies::ReadMethod::ForkPage,
                    json!({"operation_id":operation,"offset":cursor}),
                )
                .await
                .map_err(read_error)?;
            if page.offset != cursor
                || events.arrays.is_empty()
                || page.next_offset != cursor + events.arrays.len() as u64
                || page.next_offset > reply.count
            {
                return Err(invalid("invalid transformed journal page"));
            }
            let next = prepare(
                self.store.as_ref(),
                &self.handle,
                DomainRevision::default(),
                &journal,
                events,
                reply.checkpoint.clone(),
                self.read_budget.clone().into(),
            )
            .await
            .map_err(journal_error)?;
            journal = self
                .handle
                .decode(next.proposal().snapshot())
                .map_err(invalid)?;
            prepared = next;
            cursor = page.next_offset;
        }
        Ok(prepared.proposal().clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pure_transfer_preserves_capacity_and_closed_dependencies() {
        assert!(matches!(
            read_error(rsi_extension::Error::Capacity),
            ContributionError::Capacity
        ));
        assert!(matches!(
            journal_error(crate::Error::Capacity),
            ContributionError::Capacity
        ));
        assert!(matches!(
            transport_error(rsi_extension::RequestFailure::NotDispatched(
                rsi_extension::Error::Capacity
            )),
            ContributionError::Capacity
        ));
        for cause in [
            rsi_extension::Error::Cancelled,
            rsi_extension::Error::Disconnected,
        ] {
            assert!(matches!(read_error(cause), ContributionError::Closed));
        }
        assert!(matches!(
            read_error(rsi_extension::Error::Protocol),
            ContributionError::Invalid(_)
        ));
    }
}
