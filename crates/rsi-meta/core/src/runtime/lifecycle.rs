#![allow(clippy::wildcard_imports)] // This is one implementation partition of runtime.

use super::*;

impl Runtime {
    pub(super) async fn rollback_loading(&self, fiber: &Arc<Fiber>) -> CleanupReport {
        let generation = {
            let data = fiber.data.lock().expect("fiber state poisoned");
            data.generation
        };
        {
            let mut state = self.inner.state.lock().expect("runtime state poisoned");
            if let Some(staged) = state.staged_listeners.remove(&(fiber.id, generation)) {
                for listener in staged {
                    state.listener_events.remove(&listener.id);
                }
            }
        }
        self.cleanup_generation(fiber).await
    }

    pub(super) async fn unload_generation(&self, fiber: &Arc<Fiber>) -> CleanupReport {
        let (services, listener_ids, generation, published, lease) = {
            let mut data = fiber.data.lock().expect("fiber state poisoned");
            let Some(active) = data.active.as_ref() else {
                return CleanupReport::default();
            };
            let result = (
                active.services.clone(),
                active.listeners.clone(),
                active.generation,
                active.published,
                Arc::clone(&active.lease),
            );
            data.state = FiberState::Unloading;
            let snapshot = data.snapshot(fiber.id);
            fiber.watch.send_replace(snapshot);
            result
        };
        lease.close();
        let changed = if published {
            services
                .iter()
                .map(|service| service.key.clone())
                .collect::<Vec<_>>()
        } else {
            Vec::new()
        };
        {
            let mut state = self.inner.state.lock().expect("runtime state poisoned");
            let staged = state.staged_listeners.remove(&(fiber.id, generation));
            if let Some(staged) = &staged {
                for listener in staged {
                    state.listener_events.remove(&listener.id);
                }
            }
            if published {
                for binding in &services {
                    let slot = ServiceSlot {
                        key: binding.key.clone(),
                        isolation: Self::isolation_for(&fiber.base_context.isolation, &binding.key),
                    };
                    if state.providers.get(&slot).is_some_and(|current| {
                        current.provider == fiber.id && current.generation == generation
                    }) {
                        state.providers.remove(&slot);
                    }
                }
                for id in &listener_ids {
                    if let Some(event) = state.listener_events.remove(id)
                        && let Some(listeners) = state.listeners.get_mut(&event)
                    {
                        listeners.remove(*id);
                    }
                }
            }
            if published || staged.is_some() {
                state.revision += 1;
            }
        }
        self.reconcile_service_changes(&changed, Some(fiber.id))
            .await;
        lease.wait_drained().await;
        self.cleanup_generation(fiber).await
    }

    async fn cleanup_generation(&self, fiber: &Arc<Fiber>) -> CleanupReport {
        let (children, mut effects) = {
            let mut data = fiber.data.lock().expect("fiber state poisoned");
            let Some(active) = data.active.as_mut() else {
                return CleanupReport::default();
            };
            (
                std::mem::take(&mut active.children),
                std::mem::take(&mut active.effects),
            )
        };
        let mut report = CleanupReport::default();
        for child in children.into_iter().rev() {
            report.extend(self.dispose_fiber(child).await);
        }
        while let Some(effect) = effects.pop() {
            match std::panic::AssertUnwindSafe(async { (effect.cleanup)().await })
                .catch_unwind()
                .await
            {
                Ok(Ok(())) => {}
                Ok(Err(error)) => report.push(effect.label, error),
                Err(_) => report.push(effect.label, "cleanup panicked"),
            }
        }
        let mut data = fiber.data.lock().expect("fiber state poisoned");
        data.active = None;
        report
    }

    pub(super) fn dispose_fiber(&self, id: FiberId) -> BoxFuture<'static, CleanupReport> {
        let runtime = self.clone();
        Box::pin(async move {
            let fiber = {
                let state = runtime.inner.state.lock().expect("runtime state poisoned");
                state.fibers.get(&id).cloned()
            };
            let Some(fiber) = fiber else {
                return CleanupReport::default();
            };
            runtime.dispose_fiber_instance(fiber).await
        })
    }

    pub(super) fn dispose_fiber_instance(
        &self,
        fiber: Arc<Fiber>,
    ) -> BoxFuture<'static, CleanupReport> {
        let runtime = self.clone();
        Box::pin(async move {
            let id = fiber.id;
            let _configuration = fiber.configuration.lock().await;
            {
                let mut data = fiber.data.lock().expect("fiber state poisoned");
                data.disposed = true;
            }
            let _transition = fiber.transition.lock().await;
            if let Some(report) = fiber
                .data
                .lock()
                .expect("fiber state poisoned")
                .disposal_report
                .clone()
            {
                return report;
            }
            let report = runtime.unload_generation(&fiber).await;
            fiber.set_state(FiberState::Disposed);
            fiber
                .data
                .lock()
                .expect("fiber state poisoned")
                .disposal_report = Some(report.clone());
            let required_services = fiber
                .data
                .lock()
                .expect("fiber state poisoned")
                .descriptor
                .requires
                .iter()
                .map(|requirement| requirement.key.clone())
                .collect::<Vec<_>>();
            {
                let mut state = runtime.inner.state.lock().expect("runtime state poisoned");
                state.fibers.remove(&id);
                state.declarations.remove(id);
                state.pending_reconciliations.remove(&id);
                for service in required_services {
                    let remove_entry = state.dependents.get_mut(&service).is_some_and(|fibers| {
                        fibers.remove(&id);
                        fibers.is_empty()
                    });
                    if remove_entry {
                        state.dependents.remove(&service);
                    }
                }
                if let Some(parent) = fiber.parent
                    && let Some(parent_fiber) = state.fibers.get(&parent.fiber)
                {
                    let mut data = parent_fiber.data.lock().expect("fiber state poisoned");
                    if data.generation == parent.generation
                        && let Some(active) = data.active.as_mut()
                    {
                        active.children.retain(|child| *child != id);
                    }
                }
                state.revision += 1;
            }
            report
        })
    }

    /// Closes admission and joins concurrent reverse-order teardown of every root.
    ///
    /// This future must be polled inside a Tokio runtime. The first caller
    /// starts one Runtime-owned shutdown; later callers join the same report.
    pub async fn shutdown(&self) -> CleanupReport {
        if !self.inner.shutting_down.swap(true, Ordering::AcqRel) {
            let runtime = self.clone();
            tokio::spawn(async move {
                let result = std::panic::AssertUnwindSafe(runtime.shutdown_inner())
                    .catch_unwind()
                    .await;
                let report = result.unwrap_or_else(|_| {
                    let mut report = CleanupReport::default();
                    report.push(
                        "runtime shutdown".to_owned(),
                        MetaError::Activation("shutdown task panicked".to_owned()),
                    );
                    report
                });
                *runtime
                    .inner
                    .shutdown_result
                    .lock()
                    .expect("shutdown result poisoned") = Some(report);
                runtime.inner.shutdown_complete.notify_waiters();
            });
        }
        loop {
            let notified = self.inner.shutdown_complete.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if let Some(report) = self
                .inner
                .shutdown_result
                .lock()
                .expect("shutdown result poisoned")
                .clone()
            {
                return report;
            }
            notified.as_mut().await;
        }
    }

    async fn shutdown_inner(&self) -> CleanupReport {
        let roots = {
            let state = self.inner.state.lock().expect("runtime state poisoned");
            state
                .fibers
                .values()
                .filter(|fiber| fiber.parent.is_none())
                .map(|fiber| fiber.id)
                .collect::<Vec<_>>()
        };
        let deadline = tokio::time::Instant::now() + self.inner.limits.shutdown_timeout;
        let mut remaining = roots.iter().copied().collect::<BTreeSet<_>>();
        let mut disposals = roots
            .into_iter()
            .rev()
            .map(|root| {
                let runtime = self.clone();
                let disposal = tokio::spawn(async move { runtime.dispose_fiber(root).await });
                async move { (root, disposal.await) }
            })
            .collect::<FuturesUnordered<_>>();
        let mut report = CleanupReport::default();
        while !remaining.is_empty() {
            match tokio::time::timeout_at(deadline, disposals.next()).await {
                Ok(Some((root, Ok(child)))) => {
                    remaining.remove(&root);
                    report.extend(child);
                }
                Ok(Some((root, Err(error)))) => {
                    remaining.remove(&root);
                    report.push(
                        format!("fiber {} shutdown", root.0),
                        MetaError::Activation(format!("shutdown task failed: {error}")),
                    );
                }
                Ok(None) => break,
                Err(_) => {
                    self.mark_terminal("runtime shutdown timed out");
                    for root in remaining {
                        report.push(
                            format!("fiber {} shutdown", root.0),
                            MetaError::Timeout("runtime shutdown"),
                        );
                    }
                    break;
                }
            }
        }
        report
    }
}
