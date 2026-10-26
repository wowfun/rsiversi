//! Product authorization and retained lifetime around mechanical retirement.
use super::{LocalSessionService, Result, SessionError, map_store_error, map_turn_error};
use rsi_agent_store_protocol::{
    PurgeBlocker, PurgePreview, PurgeReceipt, PurgeRequest, StoreMaintenance,
};
use rsi_session_protocol::{MaintenanceKind, NavigationCleanup, SessionPurgeReceipt};
#[derive(Clone)]
pub(super) struct ActivePurge {
    request: PurgeRequest,
    origin: rsi_api_protocol::CallOrigin,
}
impl ActivePurge {
    fn same_caller(&self, origin: &rsi_api_protocol::CallOrigin) -> bool {
        use rsi_api_protocol::CallOrigin;
        match (&self.origin, origin) {
            (CallOrigin::Local, CallOrigin::Local) => true,
            (CallOrigin::Device(left), CallOrigin::Device(right)) => left.id == right.id,
            _ => false,
        }
    }
}
/// Only an unresolved admission retains these owners; no durable result is guessed.
#[derive(Debug)]
pub(super) struct PendingAdmission {
    operation: rsi_agent_store_protocol::MaintenanceOperationId,
    sessions: Vec<rsi_agent_session_protocol::SessionId>,
    closing: Vec<rsi_session_protocol::SessionClosing>,
    kernel: Vec<Box<dyn rsi_agent_turn_protocol::SessionRetirement>>,
}
impl LocalSessionService {
    fn release_retired_sessions(&self, sessions: &[rsi_agent_session_protocol::SessionId]) {
        for id in sessions {
            self.metrics.remove(id);
            self.evidence.remove(id);
            self.activity.remove(id);
            if let Some(references) = &self.references {
                references.release_session(id);
            }
        }
    }
    async fn reconcile_purge_admission(&self) -> Result<()> {
        let mut pending = self.pending_purge.lock().await;
        let Some(admission) = pending.as_ref() else {
            return Ok(());
        };
        // begin_purge has returned. This ordered Store read can now prove absence.
        let binding = self
            .maintenance_store()?
            .purge_binding(admission.operation.as_str())
            .await
            .map_err(|_| SessionError::Capacity)?;
        if binding.is_some() {
            for closing in &admission.closing {
                closing.retiring();
            }
            for reservation in &admission.kernel {
                reservation.release_residency();
            }
            self.release_retired_sessions(&admission.sessions);
        }
        // No binding: dropping Closing reopens a fresh generation. A confirmed
        // binding has already transitioned these gates to permanent retirement.
        pending.take();
        Ok(())
    }
    async fn product_receipt(&self, store: PurgeReceipt, cleanup: bool) -> SessionPurgeReceipt {
        let navigation = if !store.complete {
            NavigationCleanup::Pending
        } else if let Some(owner) = self.purge_cleanup.as_ref().and_then(|lookup| lookup()) {
            if cleanup {
                match owner.cleanup(store.operation_id.as_str()).await {
                    Ok(()) => NavigationCleanup::Complete,
                    Err(_) => NavigationCleanup::Failed,
                }
            } else {
                owner
                    .status(store.operation_id.as_str())
                    .await
                    .unwrap_or(NavigationCleanup::Failed)
            }
        } else {
            NavigationCleanup::Unavailable
        };
        SessionPurgeReceipt { store, navigation }
    }
    pub(super) fn local_maintenance_store(&self) -> Result<&dyn StoreMaintenance> {
        if !matches!(self.origin, rsi_api_protocol::CallOrigin::Local) {
            return Err(SessionError::Api(rsi_api_protocol::ApiError::Unauthorized));
        }
        self.maintenance_store()
    }
    fn maintenance_store(&self) -> Result<&dyn StoreMaintenance> {
        self.store
            .maintenance()
            .ok_or(SessionError::Api(rsi_api_protocol::ApiError::Unavailable))
    }
    fn authorize_purge_access(
        &self,
        headers: &[rsi_agent_store_protocol::StoreSessionAccess],
    ) -> Result<Vec<Option<rsi_execution::ExecutionOperation>>> {
        self.check_origin()?;
        if headers.iter().any(|header| header.protection.is_some()) {
            return Err(SessionError::Api(rsi_api_protocol::ApiError::Unauthorized));
        }
        headers
            .iter()
            .map(|header| self.admit(&header.location))
            .collect()
    }
    fn authorize_purge_locations(
        &self,
        locations: &[rsi_agent_session_protocol::ExecutionLocation],
    ) -> Result<Vec<Option<rsi_execution::ExecutionOperation>>> {
        self.check_origin()?;
        locations
            .iter()
            .map(|location| self.admit(location))
            .collect()
    }
    async fn selected_access(
        &self,
        request: &PurgeRequest,
    ) -> Result<Vec<rsi_agent_store_protocol::StoreSessionAccess>> {
        self.check_origin()?;
        let ids: Vec<_> = request
            .targets
            .iter()
            .map(|target| target.session_id.clone())
            .collect();
        let rows = self
            .store
            .session_access(&ids)
            .await
            .map_err(map_store_error)?;
        validate_access_rows(&ids, &rows).map_err(map_store_error)?;
        rows.into_iter()
            .map(|row| {
                row.map_err(map_store_error).map_err(|error| {
                    if matches!(self.origin, rsi_api_protocol::CallOrigin::Device(_))
                        && matches!(error, SessionError::NotFound(_))
                    {
                        SessionError::Api(rsi_api_protocol::ApiError::Unauthorized)
                    } else {
                        error
                    }
                })
            })
            .collect()
    }
    async fn redact(&self, preview: PurgePreview) -> Result<PurgePreview> {
        redact_sources(
            preview,
            |ids| {
                let store = self.store.clone();
                async move { store.session_access(&ids).await }
            },
            |row| {
                let token = match row.protection.as_ref() {
                    None => tokio_util::sync::CancellationToken::new(),
                    Some(scope) => self
                        .protection
                        .as_ref()
                        .ok_or(SessionError::Api(rsi_api_protocol::ApiError::Unauthorized))?
                        .view(scope, &self.origin)?,
                };
                Ok((token, self.admit(&row.location)?))
            },
            || self.check_origin(),
        )
        .await
    }
    pub(super) async fn maintenance_preview(&self, request: PurgeRequest) -> Result<PurgePreview> {
        request.validate().map_err(map_store_error)?;
        self.check_origin()?;
        self.reconcile_purge_admission().await?;
        let headers = self.selected_access(&request).await?;
        let _admission = self.authorize_purge_access(&headers)?;
        self.authorized_preview(&request, false).await
    }
    async fn authorized_preview(
        &self,
        request: &PurgeRequest,
        kernel_reserved: bool,
    ) -> Result<PurgePreview> {
        let mut preview = self
            .maintenance_store()?
            .preview_purge(request)
            .await
            .map_err(map_store_error)?;
        let ids: Vec<_> = request
            .targets
            .iter()
            .map(|target| target.session_id.clone())
            .collect();
        for id in &ids {
            if self
                .terminals
                .as_ref()
                .map(|terminals| terminals.busy(id))
                .transpose()?
                .unwrap_or(false)
                || self.lifecycles.has_hard_owners(id)
            {
                let blocker = PurgeBlocker::Busy {
                    session_id: id.clone(),
                };
                if !preview.blockers.contains(&blocker) {
                    preview.blockers.push(blocker);
                }
            }
        }
        if !kernel_reserved && let Err(busy) = self.reserve_kernel(&ids)? {
            for blocker in busy.blockers {
                if !preview.blockers.contains(&blocker) {
                    preview.blockers.push(blocker);
                }
            }
        }
        self.redact(preview).await
    }
    fn reserve_kernel(
        &self,
        ids: &[rsi_agent_session_protocol::SessionId],
    ) -> Result<
        std::result::Result<Vec<Box<dyn rsi_agent_turn_protocol::SessionRetirement>>, PurgePreview>,
    > {
        // The successful path reserves the whole selection in one Kernel census.
        // Only a refused batch needs per-Session probes for precise diagnostics.
        match self.turns.prepare_retirement(ids) {
            Ok(reservation) => return Ok(Ok(vec![reservation])),
            Err(
                rsi_agent_turn_protocol::TurnError::Capacity
                | rsi_agent_turn_protocol::TurnError::UnsettledWork(_),
            ) => {}
            Err(error) => return Err(map_turn_error(error)),
        }
        let mut reservations = Vec::with_capacity(ids.len());
        let mut blockers = Vec::new();
        for id in ids {
            match self.turns.prepare_retirement(std::slice::from_ref(id)) {
                Ok(reservation) => reservations.push(reservation),
                Err(rsi_agent_turn_protocol::TurnError::Capacity) => {
                    blockers.push(PurgeBlocker::Busy {
                        session_id: id.clone(),
                    });
                }
                Err(rsi_agent_turn_protocol::TurnError::UnsettledWork(_)) => {
                    blockers.push(PurgeBlocker::UncertainWork {
                        session_id: id.clone(),
                    });
                }
                Err(error) => return Err(map_turn_error(error)),
            }
        }
        if blockers.is_empty() {
            Ok(Ok(reservations))
        } else {
            Ok(Err(PurgePreview { blockers }))
        }
    }
    pub(super) async fn maintenance_status(
        &self,
        operation: &str,
    ) -> Result<Option<SessionPurgeReceipt>> {
        self.check_origin()?;
        self.reconcile_purge_admission().await?;
        let store = self.maintenance_store()?;
        let Some((_, locations)) = store
            .purge_binding(operation)
            .await
            .map_err(map_store_error)?
        else {
            return Ok(None);
        };
        let _admission = self.authorize_purge_locations(&locations)?;
        match store
            .purge_status(operation)
            .await
            .map_err(map_store_error)?
        {
            Some(receipt) => Ok(Some(self.product_receipt(receipt, false).await)),
            None => Ok(None),
        }
    }
    pub(super) async fn maintenance_purge(
        &self,
        request: PurgeRequest,
    ) -> Result<std::result::Result<SessionPurgeReceipt, PurgePreview>> {
        request.validate().map_err(map_store_error)?;
        self.check_origin()?;
        let Ok(permit) = self.maintenance.clone().try_acquire_owned() else {
            let active = self.active_purge.lock().expect("active purge").clone();
            let Some(active) = active.filter(|active| {
                active.same_caller(&self.origin)
                    && active.request.operation_id == request.operation_id
            }) else {
                return Err(SessionError::Capacity);
            };
            if active.request != request {
                return Err(SessionError::Invalid(
                    "purge identity reused with a different selection".into(),
                ));
            }
            return match tokio::time::timeout(
                std::time::Duration::from_mins(1),
                self.maintenance_status(request.operation_id.as_str()),
            )
            .await
            {
                Ok(Ok(Some(receipt))) => Ok(Ok(receipt)),
                Ok(Err(error)) => Err(error),
                Ok(Ok(None)) | Err(_) => Err(SessionError::MaintenanceOutcomeUnknown {
                    kind: MaintenanceKind::Purge,
                    operation_id: request.operation_id,
                }),
            };
        };
        *self.active_purge.lock().expect("active purge") = Some(ActivePurge {
            request: request.clone(),
            origin: self.origin.clone(),
        });
        let service = self.clone();
        let operation_id = request.operation_id.clone();
        // Actual work owns all guards even when the response waiter disappears.
        let (send, receive) = tokio::sync::oneshot::channel();
        drop(self.execution.spawn(async move {
            let result = service.run_purge(request).await;
            service.active_purge.lock().expect("active purge").take();
            drop(permit);
            let result = match result {
                Ok(Ok(receipt)) => Ok(Ok(service.product_receipt(receipt, true).await)),
                Ok(Err(preview)) => Ok(Err(preview)),
                Err(error) => Err(error),
            };
            let _ = send.send(result);
        }));
        await_purge(receive, operation_id).await?
    }
    #[expect(
        clippy::too_many_lines,
        reason = "purge retains authorization and retirement ownership through durable commit and reconciliation"
    )]
    async fn run_purge(
        &self,
        request: PurgeRequest,
    ) -> Result<std::result::Result<PurgeReceipt, PurgePreview>> {
        self.reconcile_purge_admission().await?;
        let uncertain =
            |error| mutation_error(error, MaintenanceKind::Purge, &request.operation_id);
        let store = self.maintenance_store()?;
        if let Some((original, locations)) = store
            .purge_binding(request.operation_id.as_str())
            .await
            .map_err(map_store_error)?
        {
            let _admission = self.authorize_purge_locations(&locations)?;
            if request != original {
                return Err(SessionError::Invalid(
                    "purge identity reused with a different selection".into(),
                ));
            }
            let receipt = store
                .purge_batch(request.operation_id.as_str())
                .await
                .map_err(uncertain)?;
            return Ok(Ok(receipt));
        }
        let headers = self.selected_access(&request).await?;
        let _admission = self.authorize_purge_access(&headers)?;
        let ids: Vec<_> = request
            .targets
            .iter()
            .map(|target| target.session_id.clone())
            .collect();
        let kernel = match self.reserve_kernel(&ids)? {
            Ok(reservations) => reservations,
            Err(preview) => return Ok(Err(preview)),
        };
        let preview = self.authorized_preview(&request, true).await?;
        if !preview.blockers.is_empty() {
            return Ok(Err(preview));
        }
        // Check every hard owner before cancelling any reader.
        let gates = ids
            .iter()
            .map(|id| self.lifecycles.gate(id))
            .collect::<Result<Vec<_>>>()?;
        for (id, gate) in ids.iter().zip(&gates) {
            if gate.has_hard_owners()
                || self
                    .terminals
                    .as_ref()
                    .map(|terminals| terminals.busy(id))
                    .transpose()?
                    .unwrap_or(false)
            {
                return Ok(Err(PurgePreview {
                    blockers: vec![PurgeBlocker::Busy {
                        session_id: id.clone(),
                    }],
                }));
            }
        }
        let closing = match close_purge_gates(&ids, &gates)? {
            Ok(closing) => closing,
            Err(preview) => return Ok(Err(preview)),
        };
        drain_readers(&closing).await?;
        self.check_origin()?;
        let receipt = match store.begin_purge(request.clone()).await {
            Ok(Ok(receipt)) => receipt,
            Ok(Err(preview)) => return Ok(Err(self.redact(preview).await?)),
            Err(error) => {
                let error = uncertain(error);
                if matches!(error, SessionError::MaintenanceOutcomeUnknown { .. }) {
                    *self.pending_purge.lock().await = Some(PendingAdmission {
                        operation: request.operation_id.clone(),
                        sessions: ids,
                        closing,
                        kernel,
                    });
                    // A failed status read retains the owners for explicit reconciliation.
                    let _ = self.reconcile_purge_admission().await;
                }
                return Err(error);
            }
        };
        for owner in &closing {
            owner.retiring();
        }
        for reservation in &kernel {
            reservation.release_residency();
        }
        self.release_retired_sessions(&ids);
        let receipt = if receipt.complete {
            receipt
        } else {
            store
                .purge_batch(request.operation_id.as_str())
                .await
                .map_err(uncertain)?
        };
        if receipt.complete {
            for owner in &closing {
                owner.purged()?;
            }
        }
        Ok(Ok(receipt))
    }
}

fn close_purge_gates(
    ids: &[rsi_agent_session_protocol::SessionId],
    gates: &[std::sync::Arc<rsi_session_protocol::SessionLifecycle>],
) -> Result<std::result::Result<Vec<rsi_session_protocol::SessionClosing>, PurgePreview>> {
    match rsi_session_protocol::SessionLifecycle::close_many(gates) {
        Ok(closing) => Ok(Ok(closing)),
        Err(SessionError::Capacity) => {
            let blockers: Vec<_> = ids
                .iter()
                .zip(gates)
                .filter(|(_, gate)| gate.has_hard_owners())
                .map(|(id, _)| PurgeBlocker::Busy {
                    session_id: id.clone(),
                })
                .collect();
            if blockers.is_empty() {
                Err(SessionError::Capacity)
            } else {
                Ok(Err(PurgePreview { blockers }))
            }
        }
        Err(error) => Err(error),
    }
}

async fn await_purge<T>(
    receive: tokio::sync::oneshot::Receiver<T>,
    operation_id: rsi_agent_store_protocol::MaintenanceOperationId,
) -> Result<T> {
    tokio::time::timeout(std::time::Duration::from_mins(1), receive)
        .await
        .ok()
        .and_then(std::result::Result::ok)
        .ok_or(SessionError::MaintenanceOutcomeUnknown {
            kind: MaintenanceKind::Purge,
            operation_id,
        })
}

pub(super) fn mutation_error(
    error: rsi_agent_store_protocol::StoreError,
    kind: MaintenanceKind,
    operation_id: &rsi_agent_store_protocol::MaintenanceOperationId,
) -> SessionError {
    match error {
        rsi_agent_store_protocol::StoreError::Io(_)
        | rsi_agent_store_protocol::StoreError::Corrupt(_) => {
            SessionError::MaintenanceOutcomeUnknown {
                kind,
                operation_id: operation_id.clone(),
            }
        }
        other => map_store_error(other),
    }
}

async fn drain_readers(owners: &[rsi_session_protocol::SessionClosing]) -> Result<()> {
    tokio::time::timeout(std::time::Duration::from_secs(30), async {
        for owner in owners {
            owner.drained().await;
        }
    })
    .await
    .map_err(|_| SessionError::Capacity)
}

// Collection retains every successful policy lease until the final visibility check.
fn validate_access_rows(
    ids: &[rsi_agent_session_protocol::SessionId],
    rows: &[rsi_agent_store_protocol::Result<rsi_agent_store_protocol::StoreSessionAccess>],
) -> rsi_agent_store_protocol::Result<()> {
    if ids.len() != rows.len()
        || ids
            .iter()
            .zip(rows)
            .any(|(id, row)| row.as_ref().is_ok_and(|row| &row.session_id != id))
    {
        return Err(rsi_agent_store_protocol::StoreError::Corrupt(
            "access batch differs from requested Session identities".into(),
        ));
    }
    Ok(())
}

async fn redact_sources<G, F>(
    mut preview: PurgePreview,
    read: impl Fn(Vec<rsi_agent_session_protocol::SessionId>) -> F,
    authorize: impl Fn(
        &rsi_agent_store_protocol::StoreSessionAccess,
    ) -> Result<(tokio_util::sync::CancellationToken, G)>,
    check_origin: impl Fn() -> Result<()>,
) -> Result<PurgePreview>
where
    F: std::future::Future<
            Output = rsi_agent_store_protocol::Result<
                Vec<rsi_agent_store_protocol::Result<rsi_agent_store_protocol::StoreSessionAccess>>,
            >,
        > + Send,
{
    let ids = preview
        .blockers
        .iter()
        .filter_map(|blocker| match blocker {
            PurgeBlocker::Referenced { source, .. } => source.clone(),
            _ => None,
        })
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    if ids.is_empty() {
        return Ok(preview);
    }
    let mut admitted = std::collections::BTreeMap::new();
    for chunk in ids.chunks(256) {
        let Ok(rows) = read(chunk.to_vec()).await else {
            continue;
        };
        if validate_access_rows(chunk, &rows).is_err() {
            continue;
        }
        for row in rows.into_iter().flatten() {
            if let Ok(lease) = authorize(&row) {
                admitted.insert(row.session_id, lease);
            }
        }
    }
    check_origin()?;
    for blocker in &mut preview.blockers {
        if let PurgeBlocker::Referenced { source, .. } = blocker
            && source
                .as_ref()
                .and_then(|id| admitted.get(id))
                .is_none_or(|(token, _)| token.is_cancelled())
        {
            *source = None;
        }
    }
    Ok(preview)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn reference_redaction_retains_only_readable_authorized_unrevoked_sources() {
        use rsi_agent_session_protocol::{ExecutionLocation, SessionId};
        use rsi_agent_store_protocol::{StoreError, StoreSessionAccess};
        use std::sync::Arc;
        use tokio_util::sync::CancellationToken;
        let target = SessionId::new("target").unwrap();
        let ids: Vec<_> = (0..513)
            .map(|n| SessionId::new(format!("source-{n:03}")).unwrap())
            .collect();
        let preview = PurgePreview {
            blockers: ids
                .iter()
                .chain(&ids[..1])
                .map(|id| PurgeBlocker::Referenced {
                    source: Some(id.clone()),
                    target: target.clone(),
                })
                .collect(),
        };
        let revoked = CancellationToken::new();
        let retained = Arc::new(());
        let reads = std::sync::Mutex::new(Vec::new());
        let result = redact_sources(
            preview,
            |chunk| {
                let (reads, ids, retained, revoked) = (&reads, &ids, &retained, &revoked);
                async move {
                    reads.lock().unwrap().push(chunk.len());
                    if chunk[0] == ids[256] {
                        assert!(
                            Arc::strong_count(retained) > 1,
                            "prior execution guards survive later reads"
                        );
                        revoked.cancel();
                        return Err(StoreError::Io("batch read failed".into()));
                    }
                    Ok(chunk
                        .iter()
                        .map(|id| {
                            if id == &ids[2] {
                                Err(StoreError::Io("row read failed".into()))
                            } else {
                                Ok(StoreSessionAccess {
                                    session_id: id.clone(),
                                    location: ExecutionLocation::Local,
                                    protection: None,
                                })
                            }
                        })
                        .collect())
                }
            },
            |row| {
                if row.session_id == ids[3] {
                    return Err(SessionError::Api(rsi_api_protocol::ApiError::Unauthorized));
                }
                Ok((
                    if row.session_id == ids[1] {
                        revoked.clone()
                    } else {
                        CancellationToken::new()
                    },
                    retained.clone(),
                ))
            },
            || {
                assert!(Arc::strong_count(&retained) > 1);
                Ok(())
            },
        )
        .await
        .unwrap();
        assert_eq!(*reads.lock().unwrap(), vec![256, 256, 1]);
        assert_eq!(Arc::strong_count(&retained), 1);
        for (index, blocker) in result.blockers.iter().enumerate() {
            let id = &ids[index % ids.len()];
            let hidden = matches!(index, 1..=3 | 256..=511);
            assert_eq!(
                blocker,
                &PurgeBlocker::Referenced {
                    source: (!hidden).then(|| id.clone()),
                    target: target.clone()
                }
            );
        }
    }

    #[tokio::test]
    async fn reference_redaction_rechecks_origin_after_collection() {
        use rsi_agent_session_protocol::{ExecutionLocation, SessionId};
        use rsi_agent_store_protocol::StoreSessionAccess;
        let revoked = tokio_util::sync::CancellationToken::new();
        let source = SessionId::new("source").unwrap();
        let preview = PurgePreview {
            blockers: vec![PurgeBlocker::Referenced {
                source: Some(source.clone()),
                target: SessionId::new("target").unwrap(),
            }],
        };
        let result = redact_sources(
            preview,
            |_| {
                let (revoked, source) = (&revoked, &source);
                async move {
                    revoked.cancel();
                    Ok(vec![Ok(StoreSessionAccess {
                        session_id: source.clone(),
                        location: ExecutionLocation::Local,
                        protection: None,
                    })])
                }
            },
            |_| Ok((tokio_util::sync::CancellationToken::new(), ())),
            || {
                if revoked.is_cancelled() {
                    Err(SessionError::Api(rsi_api_protocol::ApiError::Unauthorized))
                } else {
                    Ok(())
                }
            },
        )
        .await;
        assert_eq!(
            result,
            Err(SessionError::Api(rsi_api_protocol::ApiError::Unauthorized))
        );
    }

    #[test]
    fn hard_owner_admitted_after_preview_returns_busy_without_cancelling_other_reads() {
        let ids =
            ["first", "second"].map(|id| rsi_agent_session_protocol::SessionId::new(id).unwrap());
        let gates = ids
            .each_ref()
            .map(|_| std::sync::Arc::new(rsi_session_protocol::SessionLifecycle::default()));
        assert!(gates.iter().all(|gate| !gate.has_hard_owners()));
        let read = gates[0].attach().unwrap().read().unwrap();
        let hard = gates[1].attach().unwrap().hard().unwrap();
        let preview = close_purge_gates(&ids, &gates).unwrap().unwrap_err();
        assert_eq!(
            preview.blockers,
            vec![PurgeBlocker::Busy {
                session_id: ids[1].clone()
            }]
        );
        assert!(!read.retiring().is_cancelled());
        drop(hard);
        assert!(close_purge_gates(&ids, &gates).unwrap().is_ok());
    }
    #[tokio::test(start_paused = true)]
    async fn purge_deadline_preserves_actual_worker_ownership() {
        let lane = std::sync::Arc::new(tokio::sync::Semaphore::new(1));
        let permit = lane.clone().try_acquire_owned().unwrap();
        let (send, receive) = tokio::sync::oneshot::channel::<()>();
        let result = await_purge(
            receive,
            rsi_agent_store_protocol::MaintenanceOperationId::new("original").unwrap(),
        )
        .await;
        assert_eq!(
            result,
            Err(SessionError::MaintenanceOutcomeUnknown {
                kind: MaintenanceKind::Purge,
                operation_id: rsi_agent_store_protocol::MaintenanceOperationId::new("original")
                    .unwrap(),
            })
        );
        assert!(lane.try_acquire().is_err());
        assert!(send.send(()).is_err());
        drop(permit);
        assert!(lane.try_acquire().is_ok());
    }
    #[test]
    fn uncertain_mutations_preserve_the_receipt_family_and_identity() {
        use rsi_agent_store_protocol::StoreError;
        for kind in [MaintenanceKind::Purge, MaintenanceKind::CasGc] {
            for error in [
                StoreError::Io("worker lost".into()),
                StoreError::Corrupt("acknowledgement differs".into()),
            ] {
                assert_eq!(
                    mutation_error(
                        error,
                        kind,
                        &rsi_agent_store_protocol::MaintenanceOperationId::new("original").unwrap()
                    ),
                    SessionError::MaintenanceOutcomeUnknown {
                        kind,
                        operation_id: rsi_agent_store_protocol::MaintenanceOperationId::new(
                            "original"
                        )
                        .unwrap()
                    }
                );
            }
        }
        assert_eq!(
            mutation_error(
                StoreError::Invalid("refused".into()),
                MaintenanceKind::Purge,
                &rsi_agent_store_protocol::MaintenanceOperationId::new("original").unwrap()
            ),
            SessionError::Invalid("refused".into())
        );
    }
    #[tokio::test(start_paused = true)]
    async fn retirement_drain_deadline_covers_the_whole_selection() {
        let gates = [
            std::sync::Arc::new(rsi_session_protocol::SessionLifecycle::default()),
            std::sync::Arc::new(rsi_session_protocol::SessionLifecycle::default()),
        ];
        let first = gates[0].attach().unwrap().read().unwrap();
        let _second = gates[1].attach().unwrap().read().unwrap();
        let closing = rsi_session_protocol::SessionLifecycle::close_many(&gates).unwrap();
        let start = tokio::time::Instant::now();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_secs(20)).await;
            drop(first);
        });
        assert_eq!(drain_readers(&closing).await, Err(SessionError::Capacity));
        assert_eq!(start.elapsed(), std::time::Duration::from_secs(30));
    }
}

#[cfg(test)]
#[test]
fn access_batch_requires_exact_length_and_positional_identities() {
    use rsi_agent_session_protocol::{ExecutionLocation, SessionId};
    use rsi_agent_store_protocol::{StoreError, StoreSessionAccess};
    let id = SessionId::new("selected").unwrap();
    let row = |name| {
        Ok(StoreSessionAccess {
            session_id: SessionId::new(name).unwrap(),
            location: ExecutionLocation::Local,
            protection: None,
        })
    };
    assert!(matches!(
        validate_access_rows(std::slice::from_ref(&id), &[]),
        Err(StoreError::Corrupt(_))
    ));
    assert!(validate_access_rows(std::slice::from_ref(&id), &[row("other")]).is_err());
    assert!(
        validate_access_rows(
            std::slice::from_ref(&id),
            &[row("selected"), row("selected")]
        )
        .is_err()
    );
    assert!(validate_access_rows(&[id], &[row("selected")]).is_ok());
}
