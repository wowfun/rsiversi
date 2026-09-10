use super::{Result, SessionError};
use async_trait::async_trait;
use rsi_agent_session_protocol::{SessionHeader, SessionId};
use serde::{Deserialize, Serialize};
use std::fmt;
use tokio_util::sync::CancellationToken;

/// Current Session/Header correlation, never an authentication credential.
#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SessionTarget {
    /// Exact Session identity.
    pub session_id: SessionId,
    /// Canonical Header fingerprint observed by the caller.
    pub header_key: String,
}
impl SessionTarget {
    /// Validate the bounded fingerprint before service or filesystem work.
    pub fn validate(&self) -> Result<()> {
        if self.header_key.len() != 64
            || !self
                .header_key
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err(SessionError::Invalid(
                "invalid Session Header fingerprint".into(),
            ));
        }
        Ok(())
    }
}

/// Finite read binding retaining the actual unpublished draft activity owner.
pub struct SessionReadLease {
    header: SessionHeader,
    retiring: CancellationToken,
    _activity: Box<dyn Send + Sync>,
}
impl fmt::Debug for SessionReadLease {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SessionReadLease")
            .field("session", self.header.session_id())
            .finish_non_exhaustive()
    }
}
impl SessionReadLease {
    /// Construct from an already validated Header and its provider-owned activity.
    pub fn new(
        header: SessionHeader,
        retiring: CancellationToken,
        activity: impl Send + Sync + 'static,
    ) -> Self {
        Self {
            header,
            retiring,
            _activity: Box::new(activity),
        }
    }
    /// Exact Header held at read admission.
    pub fn header(&self) -> &SessionHeader {
        &self.header
    }
    /// Cancel a read when its owning Session service retires.
    pub fn retiring(&self) -> &CancellationToken {
        &self.retiring
    }
}

/// Trusted server-side binding and finite lifetime service; it grants no API access.
#[async_trait]
pub trait SessionReads: fmt::Debug + Send + Sync + 'static {
    /// Acquire the real Header/activity after the caller's own authorization check.
    async fn acquire(&self, target: &SessionTarget) -> Result<SessionReadLease>;
}
/// Nominal Local contract available to server-side read adapters.
#[derive(Debug)]
pub struct SessionReadContract;
impl rsi_meta_contract::LocalContract for SessionReadContract {
    const KEY: &'static str = "rsi.session.read";
    type Service = dyn SessionReads;
}
