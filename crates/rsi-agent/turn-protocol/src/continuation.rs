//! Sealed live scheduling authority, independent of serializable provenance.

use crate::{
    DomainMutationReceipt, MessageReceipt, PreparedResumeSession, Result, SubmitSession, TurnError,
};
use async_trait::async_trait;
use rsi_agent_composition_protocol::AgentCompositionPin;
use rsi_agent_session_protocol::{
    ContinuationInput, ContinuationSource, DomainIdentity, DomainRequestId, DomainRevision,
    MessageId, SessionCommandInvocation, SessionHeader, SessionId,
};
use rsi_meta_contract::LocalContract;
use std::{
    fmt,
    sync::{Arc, Mutex, Weak},
};
use tokio_util::sync::CancellationToken;

/// Exact state observed by the Host before explicitly arming a controller.
#[derive(Clone, Debug)]
pub struct ContinuationBinding {
    /// Domain whose pure reserve and settle commands govern this controller.
    pub domain: DomainIdentity,
    /// Domain-selected current task identity.
    pub owner: DomainRequestId,
    /// Exact state revision; zero selects an unpublished baseline.
    pub revision: DomainRevision,
    /// Digest of the complete current snapshot, checked at arm admission.
    pub snapshot_sha256: String,
}

/// Cloneable live authority. The Kernel retains only a weak reference.
#[derive(Clone)]
pub struct ContinuationLease(Arc<LiveContinuation>);

struct LiveContinuation {
    seal: Arc<()>,
    header: SessionHeader,
    composition: AgentCompositionPin,
    binding: ContinuationBinding,
    state: Mutex<LeaseState>,
    revoked: CancellationToken,
}

#[derive(Debug)]
struct LeaseState {
    revision: DomainRevision,
    source: Option<ContinuationSource>,
    requested: bool,
    last_admitted: u64,
}

impl Drop for LiveContinuation {
    fn drop(&mut self) {
        self.revoked.cancel();
    }
}

impl ContinuationLease {
    /// Observes revocation or last-owner release without keeping authority alive.
    #[doc(hidden)]
    pub fn disarmed_token(&self) -> CancellationToken {
        self.0.revoked.clone()
    }
    /// Synchronously stops future scheduling; retained settlement remains possible.
    pub fn revoke(&self) {
        self.0.revoked.cancel();
    }
    /// Whether explicit live scheduling authority remains available.
    pub fn is_armed(&self) -> bool {
        !self.0.revoked.is_cancelled()
    }
    /// Waits until this live authority is explicitly revoked.
    pub async fn disarmed(&self) {
        self.0.revoked.cancelled().await;
    }
    /// Immutable owning Session.
    pub fn session_id(&self) -> &SessionId {
        self.0.header.session_id()
    }
    /// Immutable domain/owner binding captured when this lease was created.
    pub fn binding(&self) -> &ContinuationBinding {
        &self.0.binding
    }
    /// Current domain revision guarded by pending automatic input.
    pub fn revision(&self) -> DomainRevision {
        self.0
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .revision
    }
    /// Current scheduling demand and last admitted control position, for Kernel fairness.
    #[doc(hidden)]
    pub fn demand(&self) -> Option<u64> {
        let state = self
            .0
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        (self.is_armed() && state.requested).then_some(state.last_admitted)
    }
    /// Non-owning registry entry; it cannot keep abandoned execution armed.
    #[doc(hidden)]
    pub fn downgrade(&self) -> WeakContinuationLease {
        WeakContinuationLease(Arc::downgrade(&self.0))
    }
    /// Exact lease identity, even if a later controller has the same durable owner.
    #[doc(hidden)]
    pub fn same_lease(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
    /// Checks the frozen source selected by the latest authorized submission.
    #[doc(hidden)]
    pub fn guards(&self, source: &ContinuationSource, revision: DomainRevision) -> bool {
        let state = self
            .0
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        self.is_armed() && state.revision == revision && state.source.as_ref() == Some(source)
    }
}

impl fmt::Debug for ContinuationLease {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ContinuationLease")
            .field("session", self.session_id())
            .field("binding", self.binding())
            .field("armed", &self.is_armed())
            .finish_non_exhaustive()
    }
}

/// Weak registry handle, never serialized or exposed to remote clients.
#[doc(hidden)]
#[derive(Clone)]
pub struct WeakContinuationLease(Weak<LiveContinuation>);
impl WeakContinuationLease {
    /// Resolves only while an owning Host operation retains its lease.
    pub fn upgrade(&self) -> Option<ContinuationLease> {
        self.0.upgrade().map(ContinuationLease)
    }
}
impl fmt::Debug for WeakContinuationLease {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("WeakContinuationLease(..)")
    }
}

/// Kernel integration issuer. Tokens issued by another instance are rejected.
#[doc(hidden)]
#[derive(Clone, Debug, Default)]
pub struct ContinuationIssuer {
    seal: Arc<()>,
}

impl ContinuationIssuer {
    /// Creates a live capability after owning Kernel admission validates its binding.
    pub fn issue(
        &self,
        header: SessionHeader,
        composition: AgentCompositionPin,
        binding: ContinuationBinding,
    ) -> ContinuationLease {
        let revision = binding.revision;
        ContinuationLease(Arc::new(LiveContinuation {
            seal: self.seal.clone(),
            header,
            composition,
            binding,
            state: Mutex::new(LeaseState {
                revision,
                source: None,
                requested: false,
                last_admitted: 0,
            }),
            revoked: CancellationToken::new(),
        }))
    }
    /// Checks the issuer and the exact immutable Header and generation.
    pub fn inspect<'a>(
        &self,
        lease: &'a ContinuationLease,
    ) -> Result<(&'a SessionHeader, &'a AgentCompositionPin)> {
        if !Arc::ptr_eq(&self.seal, &lease.0.seal) {
            return Err(TurnError::Invalid(
                "continuation lease belongs to another Kernel".into(),
            ));
        }
        Ok((&lease.0.header, &lease.0.composition))
    }
    /// Advances the guarded state only after canonical command reconciliation.
    pub fn set_revision(&self, lease: &ContinuationLease, revision: DomainRevision) -> Result<()> {
        self.inspect(lease)?;
        let mut state = lease
            .0
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if revision.get() > state.revision.get() {
            state.revision = revision;
        }
        Ok(())
    }
    /// Announces eligible work without allocating or admitting an input.
    pub fn request_round(&self, lease: &ContinuationLease) -> Result<()> {
        self.inspect(lease)?;
        if !lease.is_armed() {
            return Err(TurnError::ContinuationDisarmed);
        }
        lease
            .0
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .requested = true;
        Ok(())
    }
    /// Records service only after canonical acceptance; no busy attempt consumes a turn.
    pub fn admitted_round(&self, lease: &ContinuationLease, control_seq: u64) -> Result<()> {
        self.inspect(lease)?;
        let mut state = lease
            .0
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.requested = false;
        state.last_admitted = state.last_admitted.max(control_seq);
        Ok(())
    }
    /// Records an admitted reservation, including settlement after overlapping revocation.
    /// Recording never rearms a revoked lease; `guards` still requires live authority.
    pub fn guard_source(
        &self,
        lease: &ContinuationLease,
        source: ContinuationSource,
    ) -> Result<()> {
        self.inspect(lease)?;
        lease
            .0
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .source = Some(source);
        Ok(())
    }
}

/// Host-only continuation control, separate from ordinary command and message APIs.
#[async_trait]
pub trait SessionContinuations: fmt::Debug + Send + Sync + 'static {
    /// Arms one exact state after a successful explicit application action.
    async fn arm(
        &self,
        session: SubmitSession,
        binding: ContinuationBinding,
    ) -> Result<ContinuationLease>;
    /// Retains an already disarmed owner for explicit pause/cancel reconciliation.
    /// This lease can settle or discard; it can never allocate or admit input.
    async fn retain_for_settlement(
        &self,
        session: SubmitSession,
        binding: ContinuationBinding,
    ) -> Result<ContinuationLease>;
    /// Waits for advisory subtree idleness without charging or reserving work.
    /// Runs a pure reserve callback against a private draft baseline, then commits
    /// that baseline and exact first message together. Failure leaves the draft intact.
    async fn reserve_initial(
        &self,
        lease: &ContinuationLease,
        session: rsi_agent_composition_protocol::PreparedFreshSession,
        invocation: SessionCommandInvocation,
        input: ContinuationInput,
    ) -> Result<MessageReceipt>;
    /// Final reservation admission always rechecks under its Session gate.
    async fn wait_idle(
        &self,
        lease: &ContinuationLease,
        cancellation: CancellationToken,
    ) -> Result<()>;
    /// Executes a pure internal command, preserving normal CAS and receipt reconciliation.
    /// A reservation commits its exact message atomically while idle. Busy does
    /// not revoke the lease or charge a round.
    /// A revoked retained lease may settle, but may not reserve another input.
    async fn execute(
        &self,
        lease: &ContinuationLease,
        session: PreparedResumeSession,
        invocation: SessionCommandInvocation,
        reservation: Option<ContinuationInput>,
    ) -> Result<DomainMutationReceipt>;
    /// Reads an internal canonical receipt without arming or allocating anything.
    async fn query(
        &self,
        lease: &ContinuationLease,
        request_id: &DomainRequestId,
    ) -> Result<Option<DomainMutationReceipt>>;
    /// Discards only an unclaimed automatic input owned by this lease.
    /// An existing claimed Turn is returned unchanged and is never cancelled here.
    async fn discard_if_pending(
        &self,
        lease: &ContinuationLease,
        message_id: &MessageId,
    ) -> Result<MessageReceipt>;
}

/// Published for trusted Host composition. Like other process-local Local
/// services, this is not an isolation boundary against linked plugin code.
#[derive(Debug)]
pub struct SessionContinuationsContract;
impl LocalContract for SessionContinuationsContract {
    const KEY: &'static str = "rsi.agent.session.continuations";
    type Service = dyn SessionContinuations;
}
