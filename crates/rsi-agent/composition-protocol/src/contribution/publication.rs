use super::ContributionResult;
use async_trait::async_trait;
use rsi_agent_session_protocol::{DomainStateCommit, SessionFact, SessionId};
use std::{fmt, sync::Arc};

/// Canonical evidence supplied after the Executor verifies the exact result span.
#[derive(Debug)]
pub struct PublicationReceipt<'a> {
    /// Session owning the atomic publication.
    pub session_id: &'a SessionId,
    /// Exact ordered durable Facts published with this mutation.
    pub facts: &'a [Arc<SessionFact>],
    /// Canonical control sequence and atomic domain update, when present.
    pub domain_commit: Option<(u64, &'a DomainStateCommit)>,
}

/// Exact lease-local evidence after a draft command's atomic initial-state swap.
#[derive(Debug)]
pub struct DraftPublicationReceipt<'a> {
    /// Owning unpublished Session identity.
    pub session_id: &'a SessionId,
    /// Exact invocation and successor digest retained by the draft owner.
    pub receipt: &'a rsi_agent_session_protocol::SessionCommandReceipt,
    /// Complete successor initial state, including retained external resources.
    pub baseline: &'a crate::DomainBaseline,
    /// Complete successor input batch retained until first submit.
    pub inputs: &'a [crate::PreparedCommandInput],
}
impl DraftPublicationReceipt<'_> {
    /// Digest of the complete candidate that first submission will publish.
    ///
    /// # Errors
    /// Rejects an input batch inconsistent with the candidate baseline.
    pub fn state_sha256(&self) -> ContributionResult<String> {
        Ok(
            crate::command::initial_commit(self.baseline, self.inputs)?.map_or_else(
                || self.baseline.digest().to_owned(),
                |commit| commit.request_sha256().to_owned(),
            ),
        )
    }
}

/// Notification authority separate from pure proposal construction and Tool retirement.
#[async_trait]
pub trait PublicationObserver: fmt::Debug + Send + Sync + 'static {
    /// Confirms this exact publication without invoking the Tool again.
    async fn committed(&self, receipt: &PublicationReceipt<'_>) -> ContributionResult<()>;
    /// Confirms a draft publication; execution-only observers reject this boundary.
    async fn draft_committed(&self, _: &DraftPublicationReceipt<'_>) -> ContributionResult<()> {
        Err(super::ContributionError::Invalid(
            "observer does not accept draft publication".into(),
        ))
    }
    /// Rolls back a definitely rejected mutation without retrying the callback.
    async fn rejected(&self) -> ContributionResult<()>;
    /// Marks the operation uncertain. Must be bounded, synchronous and perform no I/O.
    /// This notification cannot infer whether publication occurred.
    fn unconfirmed(&self);
}

/// Move-only retention guard; every unfinished exit notifies uncertainty.
#[derive(Debug)]
pub struct Publication {
    observer: Option<Arc<dyn PublicationObserver>>,
}
impl Publication {
    /// Retains an observer and all immutable resources owned by it.
    pub fn new(observer: Arc<dyn PublicationObserver>) -> Self {
        Self {
            observer: Some(observer),
        }
    }
    /// Delivers canonical evidence once. An error or cancelled wait remains unconfirmed.
    ///
    /// # Errors
    /// Propagates the adapter's confirmation failure.
    pub async fn committed(&mut self, receipt: &PublicationReceipt<'_>) -> ContributionResult<()> {
        if let Some(observer) = &self.observer {
            observer.committed(receipt).await?;
            self.observer = None;
        }
        Ok(())
    }

    /// Confirms the actual draft swap outside its owner's mutation admission.
    ///
    /// # Errors
    /// Propagates confirmation failure while preserving uncertainty.
    pub async fn draft_committed(
        &mut self,
        receipt: &DraftPublicationReceipt<'_>,
    ) -> ContributionResult<()> {
        if let Some(observer) = &self.observer {
            observer.draft_committed(receipt).await?;
            self.observer = None;
        }
        Ok(())
    }
    /// Delivers a definite precommit rejection once.
    ///
    /// # Errors
    /// Propagates the adapter's rollback failure.
    pub async fn rejected(&mut self) -> ContributionResult<()> {
        if let Some(observer) = &self.observer {
            observer.rejected().await?;
            self.observer = None;
        }
        Ok(())
    }
}
impl Drop for Publication {
    fn drop(&mut self) {
        if let Some(observer) = self.observer.take() {
            // Cleanup must not unwind a second time while dropping another failure.
            let _ =
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| observer.unconfirmed()));
        }
    }
}
