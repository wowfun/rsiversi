//! Process-local evidence retained independently of publication notification.
use crate::{DomainBaseline, DraftPublicationReceipt, PreparedCommandInput, PublicationReceipt};
use rsi_agent_session_protocol::{DomainStateCommit, SessionCommandReceipt, SessionId};
use std::sync::{Arc, Mutex};

/// Immutable evidence owned by the command's publication authority.
#[derive(Clone, Debug)]
pub enum CommandOutcomeEvidence {
    /// No publication can occur after this definite refusal.
    Rejected,
    /// Exact canonical control, recorded before notification.
    Durable {
        /// Owning Session.
        session_id: SessionId,
        /// Canonical domain control sequence.
        control_seq: u64,
        /// Complete atomic domain and input receipt.
        commit: DomainStateCommit,
    },
    /// Exact applied initial-state swap, recorded before notification.
    Draft {
        /// Owning unpublished Session.
        session_id: SessionId,
        /// Applied invocation and successor identity.
        receipt: SessionCommandReceipt,
        /// Complete initial-state candidate retaining its CAS resources.
        baseline: DomainBaseline,
        /// Complete held input batch.
        inputs: Vec<PreparedCommandInput>,
    },
}

/// Read-only proof; absence means unresolved, never rejection.
#[derive(Clone, Debug)]
pub struct CommandOutcome(Arc<Mutex<Option<Arc<CommandOutcomeEvidence>>>>);
impl CommandOutcome {
    /// Reads immutable evidence without callbacks, I/O or changing authority.
    pub fn evidence(&self) -> Option<Arc<CommandOutcomeEvidence>> {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }
}

/// Move-only owner transferred through command preparation and publication.
#[derive(Debug)]
pub struct CommandOutcomeOwner {
    outcome: CommandOutcome,
    reject_on_drop: std::sync::atomic::AtomicBool,
}
impl Default for CommandOutcomeOwner {
    fn default() -> Self {
        Self {
            outcome: CommandOutcome(Arc::new(Mutex::new(None))),
            reject_on_drop: std::sync::atomic::AtomicBool::new(true),
        }
    }
}
impl CommandOutcomeOwner {
    /// Lends the read-only side to the callback.
    pub fn view(&self) -> CommandOutcome {
        self.outcome.clone()
    }

    /// Call before any write whose cancellation or panic could conceal publication.
    pub fn begin_publication(&self) {
        self.reject_on_drop
            .store(false, std::sync::atomic::Ordering::Release);
    }

    fn record(&self, evidence: CommandOutcomeEvidence) {
        let mut slot = self
            .outcome
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if slot.is_none() {
            *slot = Some(Arc::new(evidence));
        }
    }

    /// Records a definite refusal, without overriding an already proven publication.
    pub fn rejected(&self) {
        self.record(CommandOutcomeEvidence::Rejected);
    }

    /// Records the exact canonical command control before notifying observers.
    pub fn committed(&self, receipt: &PublicationReceipt<'_>) {
        if let Some((control_seq, commit)) = receipt.domain_commit {
            self.record(CommandOutcomeEvidence::Durable {
                session_id: receipt.session_id.clone(),
                control_seq,
                commit: commit.clone(),
            });
        }
    }

    /// Records the exact applied draft state before notifying observers.
    pub fn draft_committed(&self, receipt: &DraftPublicationReceipt<'_>) {
        self.record(CommandOutcomeEvidence::Draft {
            session_id: receipt.session_id.clone(),
            receipt: receipt.receipt.clone(),
            baseline: receipt.baseline.clone(),
            inputs: receipt.inputs.to_vec(),
        });
    }
}
impl Drop for CommandOutcomeOwner {
    fn drop(&mut self) {
        if self
            .reject_on_drop
            .load(std::sync::atomic::Ordering::Acquire)
        {
            self.rejected();
        }
    }
}
