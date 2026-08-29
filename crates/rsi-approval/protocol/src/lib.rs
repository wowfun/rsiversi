//! Runtime-independent approval contracts.

#![deny(unsafe_code)]
#![warn(missing_docs)]
#![allow(clippy::missing_errors_doc)]

use async_trait::async_trait;
use rsi_meta_contract::LocalContract;
use serde::{Deserialize, Serialize};
use std::fmt;
use std::sync::Arc;
use thiserror::Error;
use tokio_util::sync::CancellationToken;

/// Maximum bytes in one approval field.
pub const MAXIMUM_APPROVAL_FIELD_BYTES: usize = 4 * 1024;

/// Minimal live approval request.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ApprovalRequest {
    /// Exact request/call identity.
    pub id: String,
    /// Short effect action.
    pub action: String,
    /// Human-facing reason.
    pub reason: String,
}

impl<'de> Deserialize<'de> for ApprovalRequest {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct WireRequest {
            id: String,
            action: String,
            reason: String,
        }

        let wire = WireRequest::deserialize(deserializer)?;
        let request = Self {
            id: wire.id,
            action: wire.action,
            reason: wire.reason,
        };
        request
            .validate()
            .map(|()| request)
            .map_err(serde::de::Error::custom)
    }
}

impl ApprovalRequest {
    /// Validates closed current request bounds.
    pub fn validate(&self) -> Result<()> {
        for (kind, value) in [
            ("approval id", self.id.as_str()),
            ("approval action", self.action.as_str()),
            ("approval reason", self.reason.as_str()),
        ] {
            if value.is_empty() || value.len() > MAXIMUM_APPROVAL_FIELD_BYTES {
                return Err(ApprovalError::InvalidInput(format!(
                    "{kind} must be within 1..={MAXIMUM_APPROVAL_FIELD_BYTES} bytes"
                )));
            }
        }
        Ok(())
    }
}

/// Closed approval choice.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalDecision {
    /// Permit this one request.
    AllowOnce,
    /// Refuse this request.
    Deny,
}

/// Decision with durable-safe provenance.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ApprovalOutcome {
    /// Final decision.
    pub decision: ApprovalDecision,
    /// Stable non-secret answerer identity.
    pub answerer: String,
    /// Optional bounded non-secret explanation.
    pub reason: Option<String>,
}

impl<'de> Deserialize<'de> for ApprovalOutcome {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct WireOutcome {
            decision: ApprovalDecision,
            answerer: String,
            reason: Option<String>,
        }

        let wire = WireOutcome::deserialize(deserializer)?;
        let outcome = Self {
            decision: wire.decision,
            answerer: wire.answerer,
            reason: wire.reason,
        };
        outcome
            .validate()
            .map(|()| outcome)
            .map_err(serde::de::Error::custom)
    }
}

impl ApprovalOutcome {
    /// Validates provenance bounds.
    pub fn validate(&self) -> Result<()> {
        if self.answerer.is_empty() || self.answerer.len() > MAXIMUM_APPROVAL_FIELD_BYTES {
            return Err(ApprovalError::InvalidInput(
                "approval answerer identity is empty or too large".into(),
            ));
        }
        if self
            .reason
            .as_ref()
            .is_some_and(|reason| reason.len() > MAXIMUM_APPROVAL_FIELD_BYTES)
        {
            return Err(ApprovalError::InvalidInput(
                "approval outcome reason is too large".into(),
            ));
        }
        Ok(())
    }
}

/// One live waterfall answerer. `None` means abstain.
#[async_trait]
pub trait ApprovalAnswerer: fmt::Debug + Send + Sync + 'static {
    /// Answers or abstains from one request.
    async fn answer(
        &self,
        request: ApprovalRequest,
        cancellation: CancellationToken,
    ) -> Result<Option<ApprovalOutcome>>;
}

/// Answerer registration surface.
pub trait ApprovalAnswerers: fmt::Debug + Send + Sync + 'static {
    /// Appends one answerer until the lease drops.
    fn register(&self, answerer: Arc<dyn ApprovalAnswerer>) -> Result<ApprovalLease>;
}

/// Live approval resolver.
#[async_trait]
pub trait Approval: fmt::Debug + Send + Sync + 'static {
    /// Resolves one request through the active answerer waterfall.
    async fn ask(
        &self,
        request: ApprovalRequest,
        cancellation: CancellationToken,
    ) -> Result<ApprovalOutcome>;
}

/// Nominal Local contract for [`Approval`].
#[derive(Debug)]
pub struct ApprovalContract;

impl LocalContract for ApprovalContract {
    const KEY: &'static str = "rsi.approval";
    type Service = dyn Approval;
}

/// Nominal Local contract for [`ApprovalAnswerers`].
#[derive(Debug)]
pub struct ApprovalAnswerersContract;

impl LocalContract for ApprovalAnswerersContract {
    const KEY: &'static str = "rsi.approval.answerers";
    type Service = dyn ApprovalAnswerers;
}

/// Closed approval failure taxonomy.
#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum ApprovalError {
    /// Malformed or out-of-bounds request/response.
    #[error("invalid approval value: {0}")]
    InvalidInput(String),
    /// Cooperative cancellation interrupted the waterfall.
    #[error("approval was cancelled")]
    Cancelled,
    /// Answerer failed.
    #[error("approval answerer failed: {0}")]
    Answerer(String),
}

/// Approval result.
pub type Result<T> = std::result::Result<T, ApprovalError>;

/// Opaque answerer registration lease.
pub struct ApprovalLease {
    cleanup: Option<Box<dyn FnOnce() + Send + Sync + 'static>>,
}

impl ApprovalLease {
    /// Creates a lease from one unregister action.
    pub fn new(cleanup: impl FnOnce() + Send + Sync + 'static) -> Self {
        Self {
            cleanup: Some(Box::new(cleanup)),
        }
    }
}

impl fmt::Debug for ApprovalLease {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("ApprovalLease(..)")
    }
}

impl Drop for ApprovalLease {
    fn drop(&mut self) {
        if let Some(cleanup) = self.cleanup.take() {
            cleanup();
        }
    }
}
