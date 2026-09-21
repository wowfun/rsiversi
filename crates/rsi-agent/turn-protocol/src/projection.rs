//! Independent read-only extension view capture.

use async_trait::async_trait;
use rsi_agent_session_protocol::{SessionId, SessionProjectionSnapshot};
use rsi_meta_contract::LocalContract;
use std::fmt;

/// An instantaneous observation that never resolves a cold generation.
#[derive(Clone, Debug)]
pub enum ResidentComposition {
    /// No generation is currently retained by the Kernel.
    NotResident,
    /// A separately admitted load is in progress; this read never waits for it.
    Loading,
    /// Exact Header and redacted manifest captured from one resident pin.
    Resident {
        /// Immutable Header correlation for the observed resident generation.
        header: std::sync::Arc<rsi_agent_session_protocol::SessionHeader>,
        /// Process-local effective source identity.
        source_digest: String,
        /// Optional evidence supplied by the generation provider.
        manifest: Option<std::sync::Arc<rsi_agent_composition_protocol::CompositionManifest>>,
    },
}

/// Coalesced requery hints with no durable payload or execution capability.
pub type SessionProjectionChanges = std::pin::Pin<Box<dyn futures_util::Stream<Item = ()> + Send>>;

/// A metadata-only observation of one currently retained Kernel Session.
#[derive(Clone, Debug)]
pub struct ResidentActivity {
    /// Exact resident identity.
    pub session: SessionId,
    /// Current nonterminal Turn ownership, independent of durable history.
    pub running: bool,
}
/// Bounded instantaneous roster; it confers no execution or retention authority.
#[derive(Clone, Debug)]
pub struct ResidentActivityPage {
    /// At most 64 rows in lexical Session order.
    pub entries: Vec<ResidentActivity>,
    /// More resident identities exist beyond this page.
    pub has_more: bool,
}

/// Read-only generation selection and complete derived snapshot capture.
#[async_trait]
pub trait SessionProjections: fmt::Debug + Send + Sync + 'static {
    /// Cheap durable-commit revision, without observers or pins; None disables caching.
    fn activity_revision(&self) -> Option<u64> {
        None
    }

    /// Reads the first 64 resident identities without Store I/O or generation pins.
    fn resident_activity(&self) -> crate::Result<ResidentActivityPage> {
        Err(crate::TurnError::Invalid(
            "resident activity is unavailable".into(),
        ))
    }
    /// Peeks at residency without Store reads, activation, preparation or pinning.
    fn resident_composition(&self, session_id: &SessionId) -> crate::Result<ResidentComposition>;
    /// Subscribes before capture; includes future publication and ends on retirement.
    fn watch_projection_changes(
        &self,
        session_id: &SessionId,
    ) -> crate::Result<SessionProjectionChanges>;
    /// Reads a consistent current cut without hydrating or claiming the Session.
    async fn projection_snapshot(
        &self,
        session_id: &SessionId,
    ) -> crate::Result<SessionProjectionSnapshot>;
}

/// Nominal Local contract, independently consumable from execution and commands.
#[derive(Debug)]
pub struct SessionProjectionsContract;
impl LocalContract for SessionProjectionsContract {
    const KEY: &'static str = "rsi.agent.session.projections";
    type Service = dyn SessionProjections;
}
