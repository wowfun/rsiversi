//! Caller-bound Session access, independent of draft identity and execution residency.
use super::{Arc, LocalSessionHandle, LocalSessionService, Result, SessionError};

impl LocalSessionService {
    pub(super) fn protect_header(
        &self,
        header: &rsi_agent_session_protocol::SessionHeader,
    ) -> Result<tokio_util::sync::CancellationToken> {
        match header.protection() {
            None => Ok(tokio_util::sync::CancellationToken::new()),
            Some(scope) => self
                .protection
                .as_ref()
                .ok_or(SessionError::Api(rsi_api_protocol::ApiError::Unauthorized))?
                .view(scope, &self.origin),
        }
    }

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
        handle.protection_lease_for(&self.origin)?;
        let _admission = self.admit(handle.coordinates.location())?;
        if matches!(
            (&self.origin, &handle.origin),
            (
                rsi_api_protocol::CallOrigin::Local,
                rsi_api_protocol::CallOrigin::Local
            )
        ) && !handle.private_owner
        {
            return Ok(handle.clone());
        }
        Ok(Arc::new(LocalSessionHandle {
            origin: self.origin.clone(),
            private_owner: false,
            ..(**handle).clone()
        }))
    }
}
impl LocalSessionHandle {
    pub(super) fn protection_lease_for(
        &self,
        origin: &rsi_api_protocol::CallOrigin,
    ) -> Result<tokio_util::sync::CancellationToken> {
        match &self.scope {
            None => Ok(tokio_util::sync::CancellationToken::new()),
            Some(scope) => self
                .protection
                .as_ref()
                .ok_or(SessionError::Api(rsi_api_protocol::ApiError::Unauthorized))?
                .view(scope, origin),
        }
    }
    pub(super) fn admit_mutation(&self) -> Result<Option<rsi_execution::ExecutionOperation>> {
        if self.scope.is_some() && !self.private_owner {
            return Err(SessionError::Api(rsi_api_protocol::ApiError::Unauthorized));
        }
        self.admit()
    }

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
        if !self.private_owner {
            self.protection_lease_for(&self.origin)?;
        }
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
        let mut cursor = match after {
            Some(after) => Some(
                self.recent_cursors
                    .lock()
                    .map_err(|_| SessionError::Backend("recent cursors unavailable".into()))?
                    .read(&self.origin, after)?,
            ),
            None => None,
        };
        let mut sessions = Vec::new();
        let mut leases = Vec::new();
        let mut more = false;
        for _ in 0..128 {
            let page = self
                .store
                .list_recent_sessions(visibility.locations(), cursor.as_ref(), limit)
                .await
                .map_err(super::map_store_error)?;
            page.validate().map_err(super::map_store_error)?;
            more = page.has_more;
            if page.sessions.is_empty() {
                break;
            }
            let count = page.sessions.len();
            for (index, row) in page.sessions.into_iter().enumerate() {
                cursor = Some(row.cursor());
                match self.protect_header(&row.header) {
                    Ok(lease) => {
                        leases.push(lease);
                        sessions.push(super::SessionSummary { header: row.header });
                    }
                    Err(SessionError::Api(rsi_api_protocol::ApiError::Unauthorized)) => {}
                    Err(_) => {
                        return Err(SessionError::Api(rsi_api_protocol::ApiError::Unavailable));
                    }
                }
                if sessions.len() == limit {
                    more |= index + 1 < count;
                    break;
                }
            }
            if sessions.len() == limit || !more {
                break;
            }
        }
        self.check_origin()?;
        if leases
            .iter()
            .any(tokio_util::sync::CancellationToken::is_cancelled)
        {
            return Err(SessionError::Api(rsi_api_protocol::ApiError::Unauthorized));
        }
        let mut book = self
            .recent_cursors
            .lock()
            .map_err(|_| SessionError::Backend("recent cursors unavailable".into()))?;
        let next =
            if more {
                let visible = sessions
                    .last()
                    .map(super::SessionSummary::position)
                    .or_else(|| after.and_then(|c| c.after.clone()));
                Some(book.issue(
                    &self.origin,
                    cursor.ok_or_else(|| {
                        SessionError::Backend("recent scan did not advance".into())
                    })?,
                    visible,
                    after,
                )?)
            } else {
                if let Some(after) = after {
                    book.retire(&self.origin, after);
                }
                None
            };
        Ok(super::RecentSessionPage {
            sessions,
            has_more: more,
            next,
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
        let protected = self.protection_lease_for(&origin).unwrap_or_else(|_| {
            let token = tokio_util::sync::CancellationToken::new();
            token.cancel();
            token
        });
        let revoked = match &origin {
            rsi_api_protocol::CallOrigin::Local => tokio_util::sync::CancellationToken::new(),
            rsi_api_protocol::CallOrigin::Device(device) => device.revoked.clone(),
        };
        Box::pin(async_stream::try_stream! {
            let mut stream = Box::pin(stream);
            loop {
                let item = tokio::select! {
                    biased;
                    () = protected.cancelled() => Err(SessionError::Api(rsi_api_protocol::ApiError::Unauthorized)),
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

#[derive(Default)]
pub(super) struct RecentCursors {
    cuts: std::collections::VecDeque<RecentCut>,
}
struct RecentCut {
    principal: String,
    issued: super::RecentSessionCursor,
    cut: super::StoreRecentSessionCursor,
    expires: tokio::time::Instant,
}
impl RecentCursors {
    fn principal(origin: &rsi_api_protocol::CallOrigin) -> String {
        match origin {
            rsi_api_protocol::CallOrigin::Local => "local".into(),
            rsi_api_protocol::CallOrigin::Device(d) => format!("device:{}", d.id.as_str()),
        }
    }
    fn read(
        &self,
        origin: &rsi_api_protocol::CallOrigin,
        cursor: &super::RecentSessionCursor,
    ) -> Result<super::StoreRecentSessionCursor> {
        cursor.validate()?;
        self.cuts
            .iter()
            .find(|c| {
                c.principal == Self::principal(origin)
                    && &c.issued == cursor
                    && c.expires > tokio::time::Instant::now()
            })
            .map(|c| c.cut.clone())
            .ok_or_else(|| {
                SessionError::Invalid(
                    "recent cursor expired or belongs to another caller; refresh explicitly".into(),
                )
            })
    }
    fn retire(
        &mut self,
        origin: &rsi_api_protocol::CallOrigin,
        cursor: &super::RecentSessionCursor,
    ) {
        let principal = Self::principal(origin);
        self.cuts
            .retain(|cut| cut.principal != principal || &cut.issued != cursor);
    }
    fn release(
        &mut self,
        origin: &rsi_api_protocol::CallOrigin,
        cursor: &super::RecentSessionCursor,
    ) -> Result<()> {
        self.read(origin, cursor)?;
        self.retire(origin, cursor);
        Ok(())
    }
    fn issue(
        &mut self,
        origin: &rsi_api_protocol::CallOrigin,
        cut: super::StoreRecentSessionCursor,
        visible: Option<rsi_session_protocol::RecentSessionPosition>,
        predecessor: Option<&super::RecentSessionCursor>,
    ) -> Result<super::RecentSessionCursor> {
        let now = tokio::time::Instant::now();
        self.cuts.retain(|c| c.expires > now);
        if let Some(cursor) = predecessor {
            self.read(origin, cursor)?;
        }
        let principal = Self::principal(origin);
        let own = self
            .cuts
            .iter()
            .filter(|c| c.principal == principal)
            .count();
        if self.cuts.len() == 128 && own == 0 {
            return Err(SessionError::Capacity);
        }
        let mut entropy = [0u8; 16];
        getrandom::fill(&mut entropy)
            .map_err(|_| SessionError::Backend("cursor entropy unavailable".into()))?;
        let issued = super::RecentSessionCursor {
            token: hex::encode(entropy),
            after: visible,
        };
        if let Some(cursor) = predecessor {
            self.release(origin, cursor)?;
        } else if own >= 8 || self.cuts.len() == 128 {
            let index = self
                .cuts
                .iter()
                .position(|c| c.principal == principal)
                .expect("caller owns a cut");
            self.cuts.remove(index);
        }
        self.cuts.push_back(RecentCut {
            principal,
            issued: issued.clone(),
            cut,
            expires: now + std::time::Duration::from_mins(5),
        });
        Ok(issued)
    }
}

#[cfg(test)]
mod recent_tests {
    use super::*;
    use rsi_api_protocol::{AuthenticatedDevice, CallOrigin, DeviceId};
    fn origin(id: u8) -> CallOrigin {
        CallOrigin::Device(AuthenticatedDevice {
            id: DeviceId::from_bytes([id; 16]),
            revoked: tokio_util::sync::CancellationToken::new(),
        })
    }
    fn cut() -> super::super::StoreRecentSessionCursor {
        super::super::StoreRecentSessionCursor {
            created_at_ms: 1,
            session_id: rsi_agent_session_protocol::SessionId::new("hidden").unwrap(),
        }
    }
    #[tokio::test(start_paused = true)]
    async fn retained_cuts_enforce_principal_capacity_successors_and_expiry() {
        let mut book = RecentCursors::default();
        let owner = origin(1);
        let first = book.issue(&owner, cut(), None, None).unwrap();
        assert!(book.read(&origin(2), &first).is_err());
        let mut forged = first.clone();
        forged.after = Some(rsi_session_protocol::RecentSessionPosition {
            created_at_ms: 2,
            session_id: rsi_agent_session_protocol::SessionId::new("forged").unwrap(),
        });
        assert!(book.read(&owner, &forged).is_err());
        for _ in 0..8 {
            book.issue(&owner, cut(), None, None).unwrap();
        }
        assert_eq!(book.cuts.len(), 8);
        assert!(book.read(&owner, &first).is_err());
        for id in 2..=121 {
            book.issue(&origin(id), cut(), None, None).unwrap();
        }
        assert_eq!(book.cuts.len(), 128);
        let retained = book.cuts.back().unwrap().issued.clone();
        assert!(matches!(
            book.issue(&origin(122), cut(), None, None),
            Err(SessionError::Capacity)
        ));
        let next = book
            .issue(&origin(121), cut(), None, Some(&retained))
            .unwrap();
        assert_eq!(book.cuts.len(), 128);
        assert!(book.read(&origin(121), &retained).is_err());
        assert!(book.read(&origin(121), &next).is_ok());
        tokio::time::advance(std::time::Duration::from_mins(5)).await;
        assert!(book.read(&origin(121), &next).is_err());
        book.issue(&origin(122), cut(), None, None).unwrap();
        assert_eq!(book.cuts.len(), 1);
    }
    #[tokio::test(start_paused = true)]
    async fn completed_read_retires_an_expired_cut_without_authorizing_a_successor() {
        let mut book = RecentCursors::default();
        let owner = origin(1);
        let token = book.issue(&owner, cut(), None, None).unwrap();
        book.read(&owner, &token).unwrap();
        tokio::time::advance(std::time::Duration::from_secs(301)).await;
        assert!(book.read(&owner, &token).is_err());
        book.retire(&origin(2), &token);
        assert_eq!(book.cuts.len(), 1);
        book.retire(&owner, &token);
        assert!(book.cuts.is_empty());
    }
}
