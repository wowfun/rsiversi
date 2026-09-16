//! Finite I/O owned by the exact immutable composition, independent of projections.

use crate::{AgentCompositionPin, ContributionError, ContributionKind, ContributionResult};
use async_trait::async_trait;
use futures_util::FutureExt as _;
use rsi_agent_session_protocol::{
    SessionHeader, SessionResourceRequest, SessionResourceResponse, SessionResourceValue,
    ValidatedResourceRequest, ValidatedResourceResponse,
};
use std::{fmt, sync::Arc, time::Duration};
use tokio_util::sync::CancellationToken;

/// Read-only resource provider captured during composition registration.
#[async_trait]
pub trait SessionResourceReader: fmt::Debug + Send + Sync + 'static {
    /// Lists human-visible metadata when `id` is absent, or reads one exact body.
    /// The Header comes from the actual Session; no caller-supplied role exists.
    async fn read(
        &self,
        header: &SessionHeader,
        id: Option<&str>,
        cancellation: CancellationToken,
    ) -> ContributionResult<SessionResourceValue>;
}

/// Central read driver retaining the exact generation until completion.
#[derive(Clone, Debug)]
pub struct SessionResourceAdapter {
    pin: AgentCompositionPin,
}
impl SessionResourceAdapter {
    /// Captures a draft or resident generation, without resolving current sources.
    pub const fn new(pin: AgentCompositionPin) -> Self {
        Self { pin }
    }

    /// Executes one bounded, cancellable operation without mutation authority.
    /// Reads a bounded resource through its owning contribution.
    ///
    /// # Errors
    /// Rejects invalid requests, unavailable readers, cancellation and invalid responses.
    pub async fn read(
        &self,
        header: Arc<SessionHeader>,
        request: ValidatedResourceRequest,
        execution: &rsi_meta::Execution,
        cancellation: CancellationToken,
    ) -> ContributionResult<ValidatedResourceResponse> {
        let request = request.into_request();
        if header.agent_preset_id() != self.pin.preset_id() {
            return Err(ContributionError::Invalid(
                "resource Header does not match its generation".into(),
            ));
        }
        let deadline = execution.deadline_after(Duration::from_secs(30));
        let value = match &request {
            SessionResourceRequest::Sources => SessionResourceValue::Sources {
                sources: self
                    .pin
                    .contributions()
                    .entries()
                    .iter()
                    .filter(|entry| matches!(entry.kind(), ContributionKind::ResourceRead(_)))
                    .map(|entry| entry.id().clone())
                    .collect(),
            },
            SessionResourceRequest::List { source }
            | SessionResourceRequest::Read { source, .. } => {
                let reader = self
                    .pin
                    .contributions()
                    .entries()
                    .iter()
                    .find_map(|entry| {
                        if entry.id() != source {
                            return None;
                        }
                        if let ContributionKind::ResourceRead(reader) = entry.kind() {
                            Some(reader)
                        } else {
                            None
                        }
                    })
                    .ok_or_else(|| {
                        ContributionError::Invalid(
                            "resource provider is unavailable in this Session".into(),
                        )
                    })?;
                let id = match &request {
                    SessionResourceRequest::Read { id, .. } => Some(id.as_str()),
                    _ => None,
                };
                let stop = cancellation.child_token();
                let _guard = stop.clone().drop_guard();
                cancellation
                    .run_until_cancelled(deadline.timeout(
                        std::panic::AssertUnwindSafe(reader.read(&header, id, stop)).catch_unwind(),
                    ))
                    .await
                    .ok_or(ContributionError::Closed)?
                    .map_err(|_| {
                        ContributionError::Invalid("resource read deadline elapsed".into())
                    })?
                    .map_err(|_| {
                        ContributionError::Invalid("resource provider panicked".into())
                    })??
            }
        };
        if cancellation.is_cancelled() {
            return Err(ContributionError::Closed);
        }
        let response = SessionResourceResponse {
            session_id: header.session_id().clone(),
            header_sha256: header.fingerprint().map_err(invalid)?,
            composition_sha256: self.pin.source_digest().to_owned(),
            request,
            value,
        };
        response.validated().map_err(invalid)
    }
}
fn invalid(error: impl fmt::Display) -> ContributionError {
    ContributionError::Invalid(error.to_string())
}
