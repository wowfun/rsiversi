//! Exact live-owner admission, using the mutation submission locks.
use super::*;
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
        guards.push(SubmissionGuard {
            _guard: guard,
            _key: key,
        });
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
                        || session.pending_domain_baseline.is_some()
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
