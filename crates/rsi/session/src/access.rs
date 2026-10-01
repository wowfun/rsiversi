//! Caller-bound Session access, independent of draft identity and execution residency.
use super::{Arc, LocalSessionHandle, LocalSessionService, Result, SessionError};

impl LocalSessionService {
    pub(super) fn check_origin(&self) -> Result<()> {
        if let rsi_api_protocol::CallOrigin::Device(device) = &self.origin
            && device.revoked.is_cancelled()
        {
            return Err(SessionError::Api(rsi_api_protocol::ApiError::Unauthorized));
        }
        Ok(())
    }
    pub(super) fn admit(
        &self,
        location: &rsi_workspace_protocol::ExecutionLocation,
    ) -> Result<Option<rsi_execution::ExecutionOperation>> {
        admit_execution(self.resolver.as_ref(), &self.origin, location)
    }
    pub(super) fn bind_handle(
        &self,
        handle: &Arc<LocalSessionHandle>,
    ) -> Result<Arc<LocalSessionHandle>> {
        let _admission = self.admit(handle.coordinates.location())?;
        if matches!(
            (&self.origin, &handle.origin),
            (
                rsi_api_protocol::CallOrigin::Local,
                rsi_api_protocol::CallOrigin::Local
            )
        ) {
            return Ok(handle.clone());
        }
        Ok(Arc::new(LocalSessionHandle {
            origin: self.origin.clone(),
            ..(**handle).clone()
        }))
    }
}
impl LocalSessionHandle {
    pub(super) fn execution_lease(&self) -> Result<Option<rsi_execution::ExecutionLease>> {
        if let Some(resolver) = &self.resolver {
            resolver
                .lease(self.origin.clone(), self.coordinates.location())
                .map(Some)
                .map_err(SessionError::Api)
        } else {
            require_native(self.coordinates.location())?;
            Ok(None)
        }
    }
    pub(super) fn bind_submission(
        session: rsi_agent_turn_protocol::SubmitSession,
        execution: Option<rsi_execution::ExecutionLease>,
    ) -> Result<rsi_agent_turn_protocol::SubmitSession> {
        let Some(execution) = execution else {
            require_native(session.header().coordinates().location())?;
            return Ok(session);
        };
        session
            .with_execution(execution)
            .map_err(super::map_turn_error)
    }
    pub(super) fn admit(&self) -> Result<Option<rsi_execution::ExecutionOperation>> {
        admit_execution(
            self.resolver.as_ref(),
            &self.origin,
            self.coordinates.location(),
        )
    }
}
fn admit_execution(
    resolver: Option<&Arc<dyn rsi_execution::ExecutionResolver>>,
    origin: &rsi_api_protocol::CallOrigin,
    location: &rsi_workspace_protocol::ExecutionLocation,
) -> Result<Option<rsi_execution::ExecutionOperation>> {
    if let rsi_api_protocol::CallOrigin::Device(device) = origin
        && device.revoked.is_cancelled()
    {
        return Err(SessionError::Api(rsi_api_protocol::ApiError::Unauthorized));
    }
    match resolver {
        Some(resolver) => resolver
            .admit(origin, location)
            .map(Some)
            .map_err(SessionError::Api),
        None if *location == rsi_workspace_protocol::ExecutionLocation::Local => Ok(None),
        None => Err(SessionError::Api(rsi_api_protocol::ApiError::Unavailable)),
    }
}

pub(super) fn require_native(location: &rsi_workspace_protocol::ExecutionLocation) -> Result<()> {
    if *location == rsi_workspace_protocol::ExecutionLocation::Local {
        Ok(())
    } else {
        Err(SessionError::Api(rsi_api_protocol::ApiError::Unavailable))
    }
}
impl LocalSessionService {
    pub(super) fn visibility(&self) -> Result<rsi_execution::ExecutionVisibility> {
        self.check_origin()?;
        match &self.resolver {
            Some(resolver) => resolver.visibility(&self.origin).map_err(SessionError::Api),
            None => Ok(rsi_execution::ExecutionVisibility::new(
                rsi_execution::ExecutionLocations::only(std::collections::BTreeSet::from([
                    rsi_workspace_protocol::ExecutionLocation::Local,
                ]))
                .expect("one location"),
                rsi_execution::ExecutionOperation::new(()),
            )),
        }
    }
    pub(super) async fn visible_recent(
        &self,
        after: Option<&super::RecentSessionCursor>,
        limit: usize,
    ) -> Result<super::RecentSessionPage> {
        let visibility = self.visibility()?;
        let cursor = after.map(|cursor| super::StoreRecentSessionCursor {
            created_at_ms: cursor.created_at_ms,
            session_id: cursor.session_id.clone(),
        });
        let page = self
            .store
            .list_recent_sessions(visibility.locations(), cursor.as_ref(), limit)
            .await
            .map_err(super::map_store_error)?;
        Ok(super::RecentSessionPage {
            sessions: page
                .sessions
                .into_iter()
                .map(|row| super::SessionSummary { header: row.header })
                .collect(),
            has_more: page.has_more,
        })
    }
}
impl LocalSessionHandle {
    pub(super) fn guard_stream<T: Send + 'static>(
        &self,
        stream: impl futures_util::Stream<Item = Result<T>> + Send + 'static,
    ) -> std::pin::Pin<Box<dyn futures_util::Stream<Item = Result<T>> + Send>> {
        use futures_util::StreamExt as _;
        // Retain only access, never the draft handle or composition pin.
        let resolver = self.resolver.clone();
        let origin = self.origin.clone();
        let location = self.coordinates.location().clone();
        let revoked = match &origin {
            rsi_api_protocol::CallOrigin::Local => tokio_util::sync::CancellationToken::new(),
            rsi_api_protocol::CallOrigin::Device(device) => device.revoked.clone(),
        };
        Box::pin(async_stream::try_stream! {
            let mut stream = Box::pin(stream);
            loop {
                let item = tokio::select! {
                    biased;
                    () = revoked.cancelled() => Err(SessionError::Api(rsi_api_protocol::ApiError::Unauthorized)),
                    item = stream.next() => Ok(item),
                }?;
                let Some(item) = item else { break; };
                drop(admit_execution(resolver.as_ref(), &origin, &location)?);
                yield item?;
            }
        })
    }
}
