//! Exact live-owner admission, using the mutation submission locks.
use super::*;

struct Callback {
    kernel: AgentKernel,
    key: Arc<SubmissionKey>,
    stop: CancellationToken,
}
impl fmt::Debug for Callback {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("SessionMaintenance")
            .field(&self.key.id)
            .finish()
    }
}
impl rsi_agent_turn_protocol::SessionMaintenance for Callback {
    fn cancellation(&self) -> CancellationToken {
        self.stop.clone()
    }
}
impl Drop for Callback {
    fn drop(&mut self) {
        let owner = self
            .key
            .maintenance
            .lock()
            .expect("maintenance lock")
            .take();
        if let Some(MaintenanceOwner::Callback(callback)) = owner {
            callback.finished.cancel();
            if callback.deferred {
                self.kernel.request_ready_scan();
            }
        }
    }
}

pub(super) async fn begin_callback(
    kernel: &AgentKernel,
    id: &SessionId,
) -> TurnResult<Box<dyn rsi_agent_turn_protocol::SessionMaintenance>> {
    let admission = &kernel.inner.submission_admission;
    let key = admission.register(id);
    let _guard = key
        .mutex
        .clone()
        .try_lock_owned()
        .map_err(|_| TurnError::SessionBusy)?;
    if key.maintenance.lock().expect("maintenance lock").is_some() {
        return Err(TurnError::SessionBusy);
    }
    if !driver_idle(kernel, id).await? {
        return Err(TurnError::SessionBusy);
    }
    let stop = admission.closed.child_token();
    if stop.is_cancelled() {
        return Err(TurnError::ShuttingDown);
    }
    *key.maintenance.lock().expect("maintenance lock") =
        Some(MaintenanceOwner::Callback(CallbackState {
            stop: stop.clone(),
            finished: CancellationToken::new(),
            deferred: false,
        }));
    Ok(Box::new(Callback {
        kernel: kernel.clone(),
        key,
        stop,
    }))
}

// The caller holds this Session's submission admission. A draft has no Store row.
pub(super) async fn driver_idle(kernel: &AgentKernel, id: &SessionId) -> TurnResult<bool> {
    kernel.flush_resident_admitted(id).await?;
    match kernel.inner.store.read_inbox_control_state(id, false).await {
        Ok(state) => {
            Ok(!state.has_open_turn && !state.has_active_activation && !state.has_waking_message)
        }
        Err(StoreError::NotFound(_)) => Ok(true),
        Err(error) => Err(turn_store_error(error)),
    }
}

struct Retirement {
    kernel: AgentKernel,
    ids: Vec<SessionId>,
    _guards: Vec<SubmissionGuard>,
}
impl fmt::Debug for Retirement {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SessionRetirement")
            .field("sessions", &self.ids)
            .finish_non_exhaustive()
    }
}
impl rsi_agent_turn_protocol::SessionRetirement for Retirement {
    fn release_residency(&self) {
        let mut state = lock_state(&self.kernel.inner);
        for id in &self.ids {
            state.sessions.remove(id);
            self.kernel.inner.execution_messages.remove_session(id);
            self.kernel
                .inner
                .evidence_cache
                .lock()
                .expect("evidence cache poisoned")
                .remove_session(id);
        }
        drop(state);
        for id in &self.ids {
            self.kernel.inner.emit_activity(
                rsi_agent_turn_protocol::SessionActivityEvent::Retired(id.clone()),
            );
            self.kernel.inner.session_changes.committed(id);
        }
        self.kernel.inner.session_changes.domains_retired();
    }
}
pub(super) fn reserve(
    kernel: &AgentKernel,
    ids: &[SessionId],
) -> TurnResult<Box<dyn rsi_agent_turn_protocol::SessionRetirement>> {
    if ids.is_empty() || ids.len() > 256 || ids.windows(2).any(|p| p[0] >= p[1]) {
        return Err(TurnError::Invalid(
            "retirement requires sorted distinct Session IDs".into(),
        ));
    }
    let mut guards = Vec::with_capacity(ids.len());
    for id in ids {
        let key = kernel.inner.submission_admission.register(id);
        let guard = key
            .mutex
            .clone()
            .try_lock_owned()
            .map_err(|_| TurnError::Capacity)?;
        if key.maintenance.lock().expect("maintenance lock").is_some() {
            return Err(TurnError::Capacity);
        }
        guards.push(SubmissionGuard { _guard: guard, key });
    }
    {
        let state = lock_state(&kernel.inner);
        if !state.accepting {
            return Err(TurnError::ShuttingDown);
        }
        for id in ids {
            if state.controlled_work.unsettled(id) {
                return Err(TurnError::UnsettledWork(id.to_string()));
            }
        }
        if kernel.inner.commands.contains_any(ids) {
            return Err(TurnError::Capacity);
        }
        for id in ids {
            if state.loading_sessions.contains_key(id)
                || state.fresh_reservations.contains(id)
                || state.controlled_work.busy(id)
                || state.queued.iter().any(|(session, _)| session == id)
                || state.sessions.get(id).is_some_and(|session| {
                    session.header_pending
                        || !session.pending.is_empty()
                        || !session.pending_initial_controls.is_empty()
                        || session.flush_inflight
                        || session.permanent_flush_error.is_some()
                        || session.admission_reservations != 0
                        || !session.turns.is_empty()
                })
            {
                return Err(TurnError::Capacity);
            }
        }
    }
    let programs = kernel
        .inner
        .programs
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if ids
        .iter()
        .any(|id| programs.get(id).and_then(Weak::upgrade).is_some())
    {
        return Err(TurnError::Capacity);
    }
    drop(programs);
    let continuations = kernel
        .inner
        .continuations
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if continuations
        .iter()
        .any(|((id, _), lease)| ids.binary_search(id).is_ok() && lease.upgrade().is_some())
    {
        return Err(TurnError::Capacity);
    }
    drop(continuations);
    Ok(Box::new(Retirement {
        kernel: kernel.clone(),
        ids: ids.to_vec(),
        _guards: guards,
    }))
}
