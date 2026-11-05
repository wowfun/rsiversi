use super::*;
use rsi_agent_composition_protocol::{
    ContributionError, ContributionFactPage, ContributionFactReader, ContributionResult,
};

#[derive(Debug)]
struct Facts {
    kernel: std::sync::Weak<KernelInner>,
    session: SessionId,
    through: u64,
    cancellation: CancellationToken,
}
#[async_trait]
impl ContributionFactReader for Facts {
    async fn read(&self, after_seq: u64, limit: usize) -> ContributionResult<ContributionFactPage> {
        if self.cancellation.is_cancelled() {
            return Err(ContributionError::Closed);
        }
        let inner = self.kernel.upgrade().ok_or(ContributionError::Closed)?;
        if after_seq > self.through || limit == 0 || limit > MAXIMUM_FACTS_PER_READ {
            return Err(ContributionError::Invalid(
                "captured Fact read is out of bounds".into(),
            ));
        }
        if after_seq == self.through {
            return Ok(ContributionFactPage {
                facts: vec![],
                through_seq: after_seq,
            });
        }
        let (page, _admission) =
            observation::read_facts_bounded(&inner, &self.session, after_seq, limit)
                .await
                .map_err(|_| ContributionError::Closed)?;
        if self.cancellation.is_cancelled() {
            return Err(ContributionError::Closed);
        }
        let facts: Vec<_> = page
            .facts
            .into_iter()
            .take_while(|fact| fact.seq() <= self.through)
            .map(Arc::new)
            .collect();
        let mut cursor = after_seq;
        for fact in &facts {
            if fact.seq() != cursor + 1 {
                return Err(ContributionError::Invalid(
                    "captured Fact prefix is not contiguous".into(),
                ));
            }
            cursor = fact.seq();
        }
        if cursor == after_seq {
            return Err(ContributionError::Invalid(
                "captured Fact prefix is absent".into(),
            ));
        }
        Ok(ContributionFactPage {
            facts,
            through_seq: cursor,
        })
    }
}
pub(super) fn capture(
    kernel: &AgentKernel,
    session: &SessionId,
    through_seq: u64,
    cancellation: CancellationToken,
) -> Arc<dyn ContributionFactReader> {
    Arc::new(Facts {
        kernel: Arc::downgrade(&kernel.inner),
        session: session.clone(),
        through: through_seq,
        cancellation,
    })
}
