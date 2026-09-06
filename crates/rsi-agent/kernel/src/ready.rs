use super::*;

#[derive(Default)]
pub(super) struct ReadySchedulerState {
    cursor: Option<SessionId>,
    pub(super) health: rsi_agent_turn_protocol::ReadySchedulerHealth,
    error: Option<TurnError>,
    page: VecDeque<SessionId>,
    fetching: Option<u64>,
    retry_after: Option<Instant>,
    generation: u64,
    rescan_requested: bool,
    preparing: BTreeMap<SessionId, u64>,
}

struct ReadyReservation {
    inner: Weak<KernelInner>,
    root: Option<SessionId>,
    generation: u64,
}

impl Drop for ReadyReservation {
    fn drop(&mut self) {
        let Some(inner) = self.inner.upgrade() else {
            return;
        };
        let mut scheduler = inner
            .ready_activation
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(root) = &self.root {
            if scheduler.preparing.get(root) == Some(&self.generation) {
                scheduler.preparing.remove(root);
            }
        } else if scheduler.fetching == Some(self.generation) {
            scheduler.fetching = None;
        }
        if !scheduler.page.is_empty()
            || scheduler.rescan_requested
            || (scheduler.cursor.is_some()
                && scheduler
                    .retry_after
                    .is_none_or(|deadline| Instant::now() >= deadline))
        {
            inner.claim_changed.notify_waiters();
        }
    }
}

impl SessionKernel {
    pub(super) fn request_ready_scan(&self) {
        let mut scheduler = self
            .inner
            .ready_activation
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        scheduler.rescan_requested = true;
        drop(scheduler);
        self.inner.claim_changed.notify_waiters();
    }

    pub(super) fn record_ready_failure(&self, error: &TurnError) {
        let mut scheduler = self
            .inner
            .ready_activation
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        scheduler.health.failures = scheduler.health.failures.saturating_add(1);
        scheduler.health.last_error = Some(bounded_diagnostic(&error.to_string()));
    }

    pub(super) fn activate_one_ready_message(
        &self,
        cancellation: &CancellationToken,
    ) -> TurnResult<()> {
        let state = lock_state(&self.inner);
        if !state.accepting || cancellation.is_cancelled() {
            return Ok(());
        }
        let mut scheduler = self
            .inner
            .ready_activation
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(error) = scheduler.error.take() {
            return Err(error);
        }
        while scheduler.preparing.len() < 4 {
            let Some(root) = scheduler.page.pop_front() else {
                break;
            };
            if scheduler.preparing.contains_key(&root) {
                continue;
            }
            scheduler.generation = scheduler
                .generation
                .checked_add(1)
                .ok_or_else(|| TurnError::Invariant("ready generation exhausted".into()))?;
            let generation = scheduler.generation;
            scheduler.preparing.insert(root.clone(), generation);
            let reservation = ReadyReservation {
                inner: Arc::downgrade(&self.inner),
                root: Some(root.clone()),
                generation,
            };
            let kernel = self.clone();
            let cancellation = cancellation.clone();
            self.inner.tasks.spawn(async move {
                let _reservation = reservation;
                if cancellation.is_cancelled() {
                    return;
                }
                let result = kernel.activate_ready_root(&root, &cancellation).await;
                if let Err(error) = &result {
                    kernel.record_ready_failure(error);
                }
                match result {
                    Ok(true) => kernel.inner.claim_changed.notify_waiters(),
                    Err(error @ TurnError::Invariant(_)) => {
                        kernel
                            .inner
                            .ready_activation
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner)
                            .error = Some(error);
                        kernel.inner.claim_changed.notify_waiters();
                    }
                    Ok(false) | Err(_) => {}
                }
            });
        }
        self.spawn_ready_page(&mut scheduler, cancellation)?;

        Ok(())
    }

    fn spawn_ready_page(
        &self,
        scheduler: &mut ReadySchedulerState,
        cancellation: &CancellationToken,
    ) -> TurnResult<()> {
        if scheduler.page.is_empty()
            && scheduler.fetching.is_none()
            && scheduler
                .retry_after
                .is_none_or(|deadline| Instant::now() >= deadline)
            && (scheduler.preparing.is_empty()
                || scheduler.cursor.is_some()
                || scheduler.rescan_requested)
        {
            scheduler.generation = scheduler
                .generation
                .checked_add(1)
                .ok_or_else(|| TurnError::Invariant("ready generation exhausted".into()))?;
            let generation = scheduler.generation;
            let cursor = scheduler.cursor.clone();
            scheduler.rescan_requested = false;
            scheduler.fetching = Some(generation);
            let reservation = ReadyReservation {
                inner: Arc::downgrade(&self.inner),
                root: None,
                generation,
            };
            let kernel = self.clone();
            let cancellation = cancellation.clone();
            self.inner.tasks.spawn(async move {
                let _reservation = reservation;
                let result = tokio::select! {
                    biased;
                    () = cancellation.cancelled() => return,
                    result = kernel.read_ready_roots(cursor.as_ref()) => result,
                };
                if !matches!(&result, Ok(Some(_))) {
                    kernel
                        .inner
                        .ready_activation
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .retry_after = Some(Instant::now() + Duration::from_secs(5));
                }
                if let Err(error) = result {
                    kernel.record_ready_failure(&error);
                    kernel
                        .inner
                        .ready_activation
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .error = Some(error);
                    kernel.inner.claim_changed.notify_waiters();
                } else if let Ok(Some(page)) = result {
                    let mut scheduler = kernel
                        .inner
                        .ready_activation
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    if scheduler.fetching == Some(generation) {
                        scheduler.retry_after = None;
                        scheduler.cursor = if page.has_more {
                            page.roots.last().cloned()
                        } else {
                            None
                        };
                        scheduler.page = page.roots.into();
                        if !scheduler.page.is_empty() {
                            kernel.inner.claim_changed.notify_waiters();
                        }
                    }
                }
            });
        }
        Ok(())
    }

    pub(super) fn reserve_tree_lane(&self, root: &SessionId) -> Option<Arc<TreeClaimLane>> {
        let mut state = lock_state(&self.inner);
        let pool = tree_pool(&mut state, root);
        let permit = Arc::clone(&pool).try_acquire_owned().ok()?;
        Some(Arc::new(TreeClaimLane {
            pool,
            permit: Mutex::new(Some(permit)),
        }))
    }
}

pub(super) fn tree_pool(state: &mut KernelState, root: &SessionId) -> Arc<Semaphore> {
    state.tree_lanes.retain(|_, pool| pool.strong_count() != 0);
    state
        .tree_lanes
        .get(root)
        .and_then(Weak::upgrade)
        .unwrap_or_else(|| {
            let pool = Arc::new(Semaphore::new(MAXIMUM_RUNNING_AGENT_TREE_NODES));
            state.tree_lanes.insert(root.clone(), Arc::downgrade(&pool));
            pool
        })
}
