//! Independent read-only extension view capture.

use async_trait::async_trait;
use rsi_agent_session_protocol::{SessionId, SessionProjectionSnapshot};
use rsi_meta_contract::LocalContract;
use std::fmt;

/// Live metadata changes emitted after durable publication. Observers authorize
/// Session visibility separately. Historical reads and replay never emit them.
#[derive(Debug)]
pub enum SessionActivityEvent {
    /// A newly admitted Turn may change running status in the caller's visible page.
    TurnStarted,
    /// A confirmed terminal Fact from the running Kernel.
    TurnEnded {
        /// Session whose turn completed.
        session: SessionId,
        /// Exact terminal position, including after acknowledgment loss.
        sequence: u64,
        /// A provider or execution failure ended the turn.
        failed: bool,
    },
    /// The Session has entered irreversible retirement.
    Retired(SessionId),
}

impl rsi_meta::LocalEvent for SessionActivityEvent {
    const KEY: &'static str = "rsi.agent.session.activity";
    type Value = Self;
    type Error = std::convert::Infallible;
    type Mode = rsi_meta::Emit;
}

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
    /// Subscribes before a directory capture. Each wake invalidates the previous activity view.
    /// Providers without publication notifications return no subscription.
    fn watch_activity(&self) -> crate::Result<Option<SessionProjectionChanges>> {
        Ok(None)
    }
    /// Coalesced hints for edits above the baseline revision or Session retirement.
    /// Subscribe before reading. Hints are Local and carry no caller visibility.
    fn watch_edited_domain(&self, _domain: &str) -> crate::Result<SessionProjectionChanges> {
        Err(crate::TurnError::Invalid(
            "domain observation unavailable".into(),
        ))
    }
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
    /// Returns the selected identities with resident nonterminal work in one observation.
    fn running_sessions(
        &self,
        sessions: &[SessionId],
    ) -> crate::Result<std::collections::BTreeSet<SessionId>>;
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
