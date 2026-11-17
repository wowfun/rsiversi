//! One reconstruction owns its authenticated intervals, not a global cache.
use super::*;
use rsi_agent_store_protocol::{
    InheritedReadError, SessionValidationLease, capture_inherited_intervals,
};
use rsi_agent_turn_protocol::ForkHistoryReader;

#[derive(Debug)]
struct Reader {
    kernel: std::sync::Weak<KernelInner>,
    claim: TurnClaim,
    checkpoint: bool,
    sources: Vec<SessionHeader>,
    _leases: Vec<SessionValidationLease>,
}

async fn admitted(kernel: &AgentKernel, claim: &TurnClaim, checkpoint: bool) -> TurnResult<bool> {
    if !checkpoint {
        kernel.validate_claim(&lock_state(&kernel.inner), claim)?;
        return Ok(true);
    }
    kernel.validate_issued_claim(claim)?;
    let stable = lock_state(&kernel.inner)
        .sessions
        .get(claim.session_id())
        .is_none_or(|session| {
            session
                .live_seq()
                .is_ok_and(|live| live == session.durable_seq)
        });
    Ok(stable
        && read_stored_outcome(&kernel.inner, claim.session_id(), claim.turn_id())
            .await?
            .is_some())
}

pub(super) async fn open(
    kernel: &AgentKernel,
    claim: &TurnClaim,
    checkpoint: bool,
) -> TurnResult<Option<Arc<dyn ForkHistoryReader>>> {
    if !admitted(kernel, claim, checkpoint).await? {
        return Ok(None);
    }
    let intervals = capture(&kernel.inner, claim.header()).await?;
    if !admitted(kernel, claim, checkpoint).await? {
        return Ok(None);
    }
    let (sources, leases) = intervals
        .into_iter()
        .map(|interval| (interval.source, interval.lease))
        .unzip();
    Ok(Some(Arc::new(Reader {
        kernel: Arc::downgrade(&kernel.inner),
        claim: claim.clone(),
        checkpoint,
        sources,
        _leases: leases,
    })))
}

#[async_trait]
impl ForkHistoryReader for Reader {
    fn sources(&self) -> &[SessionHeader] {
        &self.sources
    }

    async fn read_facts(
        &self,
        source: &SessionId,
        after_parent_seq: u64,
        limit: usize,
    ) -> TurnResult<Option<ForkFactPage>> {
        let kernel = AgentKernel {
            inner: self.kernel.upgrade().ok_or(TurnError::ShuttingDown)?,
        };
        if limit == 0 || limit > MAXIMUM_FACTS_PER_READ {
            return Err(TurnError::Invalid(
                "fork Fact read limit is out of bounds".into(),
            ));
        }
        if !admitted(&kernel, &self.claim, self.checkpoint).await? {
            return Ok(None);
        }
        let header = self
            .sources
            .iter()
            .find(|header| header.session_id() == source)
            .ok_or_else(|| {
                TurnError::Invalid("fork source is outside the immutable selection".into())
            })?;
        let origin = header
            .history_origin()
            .expect("captured source has an origin");
        if after_parent_seq < origin.resolved_after_seq
            || after_parent_seq > origin.resolved_terminal_seq
        {
            return Err(TurnError::Invalid(
                "fork parent cursor is outside the immutable inherited interval".into(),
            ));
        }
        // A proof lease prevents cache eviction, not Session retirement. Recheck
        // availability for the complete chain without decoding or rehashing Headers.
        for source in &self.sources {
            kernel
                .inner
                .store
                .read_watermarks(
                    &source
                        .history_origin()
                        .expect("captured origin")
                        .parent_session_id,
                )
                .await
                .map_err(turn_store_error)?;
        }
        let facts = if after_parent_seq == origin.resolved_terminal_seq {
            Vec::new()
        } else {
            let (page, _) = read_facts_bounded(
                &kernel.inner,
                &origin.parent_session_id,
                after_parent_seq,
                limit,
            )
            .await
            .map_err(turn_store_error)?;
            page.facts
                .into_iter()
                .take_while(|fact| fact.seq() <= origin.resolved_terminal_seq)
                .map(Arc::new)
                .collect::<Vec<_>>()
        };
        if !admitted(&kernel, &self.claim, self.checkpoint).await? {
            return Ok(None);
        }
        let through_parent_seq = facts.last().map_or(after_parent_seq, |fact| fact.seq());
        if through_parent_seq == after_parent_seq
            && after_parent_seq != origin.resolved_terminal_seq
        {
            return Err(TurnError::Invariant(
                "fork parent Fact page made no progress".into(),
            ));
        }
        Ok(Some(ForkFactPage {
            facts,
            through_parent_seq,
        }))
    }
}

pub(super) async fn capture(
    inner: &Arc<KernelInner>,
    header: &SessionHeader,
) -> TurnResult<Vec<rsi_agent_store_protocol::InheritedInterval>> {
    let session = header.session_id().clone();
    let header = header.clone();
    // Capture materializes one bounded Header at a time, as the previous walk did.
    // The dispatched task retains admission if its caller is cancelled.
    let (intervals, _permit, _lease) = store_reads::read(
        inner,
        &session,
        MAXIMUM_SESSION_HEADER_BYTES,
        false,
        move |store, _| async move {
            capture_inherited_intervals(store.as_ref(), &header)
                .await
                .map_err(|error| match error {
                    InheritedReadError::Store(error) => error,
                    other => StoreError::Corrupt(other.to_string()),
                })
        },
    )
    .await
    .map_err(turn_store_error)?;
    Ok(intervals)
}
