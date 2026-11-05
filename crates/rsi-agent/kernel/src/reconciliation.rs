use super::{
    AtomicAgentCommit, AtomicAgentCommitResult, KernelInner, MAXIMUM_FACTS_PER_READ,
    read_validated_header_bounded, store_reads,
};

impl KernelInner {
    /// A possibly appended cancellation must not leave the resident Fact cursor writable.
    pub(super) fn fail_unverified_cancellations(
        &self,
        commit: &AtomicAgentCommit,
        error: &rsi_agent_store_protocol::StoreError,
    ) {
        use super::{FlushStatus, SessionFactBody, bounded_diagnostic, lock_state};
        let mut state = lock_state(self);
        for append in &commit.sessions {
            if !append
                .facts
                .iter()
                .any(|fact| matches!(fact.body(), SessionFactBody::CancelRequested { .. }))
            {
                continue;
            }
            let Some(session) = state.sessions.get_mut(&append.session_id) else {
                continue;
            };
            session.permanent_flush_error.get_or_insert_with(|| bounded_diagnostic(
                &format!("cancellation commit requires recovery: acknowledgement and proof unavailable: {error}")
            ));
            for fact in &append.facts {
                if let SessionFactBody::CancelRequested { turn_id, .. } = fact.body()
                    && let Some(turn) = session.turns.get(turn_id)
                {
                    turn.cancellation.cancel();
                }
            }
            session.flush_status.send_replace(FlushStatus {
                durable_seq: session.durable_seq,
                permanent_error: session.permanent_flush_error.clone(),
            });
        }
        self.claim_changed.notify_waiters();
        self.process_pending_changed.notify_waiters();
    }

    pub(crate) async fn reconcile_commit(
        &self,
        commit: &AtomicAgentCommit,
    ) -> rsi_agent_store_protocol::Result<Option<AtomicAgentCommitResult>> {
        let mut sessions = Vec::with_capacity(commit.sessions.len());
        for append in &commit.sessions {
            if let Some(header) = &append.header
                && read_validated_header_bounded(self, &append.session_id).await? != *header
            {
                return Ok(None);
            }
            let fact_anchor = append
                .facts
                .last()
                .map_or(append.expected_fact_seq, |fact| fact.seq());
            let mut matched = 0;
            while matched < append.controls.len() {
                let expected = &append.controls[matched];
                let after = expected.seq() - 1;
                let ((page, _limit), _permit, _lease) = store_reads::page(
                    self,
                    &append.session_id,
                    (append.controls.len() - matched).min(MAXIMUM_FACTS_PER_READ),
                    move |store, id, limit, bytes| async move {
                        store.read_controls(&id, after, limit, bytes).await
                    },
                )
                .await?;
                page.validate()?;
                if page.after_seq != after
                    || page.records.is_empty()
                    || page.records.len() > append.controls.len() - matched
                    || page.records != append.controls[matched..matched + page.records.len()]
                    || page
                        .fact_cursors
                        .iter()
                        .any(|anchor| *anchor != fact_anchor)
                {
                    return Ok(None);
                }
                matched += page.records.len();
            }
            let mut matched = 0;
            while matched < append.facts.len() {
                let expected = &append.facts[matched];
                let after = expected.seq() - 1;
                let ((page, _limit), _permit, _lease) = store_reads::page(
                    self,
                    &append.session_id,
                    (append.facts.len() - matched).min(MAXIMUM_FACTS_PER_READ),
                    move |store, id, limit, bytes| async move {
                        store.read_facts(&id, after, limit, bytes).await
                    },
                )
                .await?;
                page.validate()?;
                if page.after_seq != after
                    || page.facts.is_empty()
                    || page.facts.len() > append.facts.len() - matched
                    || page
                        .facts
                        .iter()
                        .zip(&append.facts[matched..])
                        .any(|(actual, expected)| actual != expected.as_ref())
                {
                    return Ok(None);
                }
                matched += page.facts.len();
            }
            sessions.push(self.store.read_watermarks(&append.session_id).await?);
        }
        Ok(Some(AtomicAgentCommitResult { sessions }))
    }
}
