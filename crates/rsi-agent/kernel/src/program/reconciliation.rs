use super::{
    AtomicAgentCommit, AtomicAgentCommitResult, KernelInner, MAXIMUM_SESSION_FACT_BYTES,
    read_validated_header_bounded, store_reads,
};

impl KernelInner {
    pub(crate) async fn reconcile_program_commit(
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
            for expected in &append.controls {
                let after = expected.seq() - 1;
                let (page, _permit, _lease) = store_reads::read(
                    self,
                    &append.session_id,
                    MAXIMUM_SESSION_FACT_BYTES,
                    true,
                    move |store, id| async move { store.read_controls(&id, after, 1).await },
                )
                .await?;
                page.validate()?;
                if page.after_seq != after
                    || page.records.as_slice() != std::slice::from_ref(expected)
                {
                    return Ok(None);
                }
            }
            for expected in &append.facts {
                let after = expected.seq() - 1;
                let (page, _permit, _lease) = store_reads::read(
                    self,
                    &append.session_id,
                    MAXIMUM_SESSION_FACT_BYTES,
                    true,
                    move |store, id| async move { store.read_facts(&id, after, 1).await },
                )
                .await?;
                page.validate()?;
                if page.after_seq != after
                    || page.facts.as_slice() != std::slice::from_ref(expected.as_ref())
                {
                    return Ok(None);
                }
            }
            sessions.push(self.store.read_watermarks(&append.session_id).await?);
        }
        Ok(Some(AtomicAgentCommitResult { sessions }))
    }
}
