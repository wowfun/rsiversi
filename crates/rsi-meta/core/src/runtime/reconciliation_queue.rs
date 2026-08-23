#![allow(clippy::wildcard_imports)] // This is one implementation partition of runtime.

use super::*;

impl Runtime {
    pub(super) fn dependency_cycle(&self, start: FiberId) -> Option<Vec<ServiceKey>> {
        let mut visited = BTreeSet::from([start]);
        let mut stack = vec![(start, self.cycle_edges(start), 0_usize)];
        let mut services = Vec::new();
        while let Some((_, edges, next_edge)) = stack.last_mut() {
            let Some((next, service)) = edges.get(*next_edge).cloned() else {
                stack.pop();
                if !stack.is_empty() {
                    services.pop();
                }
                continue;
            };
            *next_edge += 1;
            if next == start {
                services.push(service);
                return Some(services);
            }
            if visited.insert(next) {
                services.push(service);
                stack.push((next, self.cycle_edges(next), 0));
            }
        }
        None
    }

    fn cycle_edges(&self, id: FiberId) -> Vec<(FiberId, ServiceKey)> {
        let fiber = {
            let state = self.inner.state.lock().expect("runtime state poisoned");
            state.fibers.get(&id).cloned()
        };
        let Some(fiber) = fiber else {
            return Vec::new();
        };
        let requirements = {
            let data = fiber.data.lock().expect("fiber state poisoned");
            if data.disposed {
                return Vec::new();
            }
            if let Some(active) = &data.active {
                return active
                    .bindings
                    .iter()
                    .map(|(service, binding)| (binding.provider, service.clone()))
                    .collect();
            }
            // Pending and Failed fibers have no actual bindings. Their
            // validated declarations remain graph edges because Failed is a
            // recoverable state: reconfiguration can reactivate the same Fiber.
            data.descriptor.requires.clone()
        };
        let state = self.inner.state.lock().expect("runtime state poisoned");
        requirements
            .iter()
            .flat_map(|requirement| {
                state
                    .declarations
                    .providers(&fiber.base_context, requirement)
                    .into_iter()
                    .map(|provider| (provider, requirement.key.clone()))
            })
            .collect()
    }

    pub(super) fn notify_service_changes(&self, services: &[ServiceKey], except: Option<FiberId>) {
        let should_spawn = {
            let mut state = self.inner.state.lock().expect("runtime state poisoned");
            let affected = Self::dependent_ids(&state, services, except);
            state.pending_reconciliations.extend(affected);
            if state.pending_reconciliations.is_empty() || state.reconciliation_worker_running {
                false
            } else {
                state.reconciliation_worker_running = true;
                true
            }
        };
        if !should_spawn {
            return;
        }
        let runtime = self.clone();
        tokio::spawn(async move { runtime.run_reconciliation_worker().await });
    }

    fn dependent_ids(
        state: &RuntimeState,
        services: &[ServiceKey],
        except: Option<FiberId>,
    ) -> BTreeSet<FiberId> {
        services
            .iter()
            .filter_map(|service| state.dependents.get(service))
            .flat_map(BTreeSet::iter)
            .copied()
            .filter(|id| Some(*id) != except)
            .collect()
    }

    async fn run_reconciliation_worker(&self) {
        loop {
            let pending = {
                let mut state = self.inner.state.lock().expect("runtime state poisoned");
                if state.pending_reconciliations.is_empty() {
                    state.reconciliation_worker_running = false;
                    return;
                }
                std::mem::take(&mut state.pending_reconciliations)
            };
            futures_util::stream::iter(pending)
                .for_each_concurrent(self.inner.limits.maximum_concurrent_reconciliations, |id| {
                    self.reconcile_fiber(id)
                })
                .await;
        }
    }

    pub(super) async fn reconcile_service_changes(
        &self,
        services: &[ServiceKey],
        except: Option<FiberId>,
    ) {
        let affected = {
            let mut state = self.inner.state.lock().expect("runtime state poisoned");
            let affected = Self::dependent_ids(&state, services, except);
            for id in &affected {
                state.pending_reconciliations.remove(id);
            }
            affected
        };
        let tasks = affected.into_iter().map(|id| {
            let runtime = self.clone();
            tokio::spawn(async move { runtime.reconcile_fiber(id).await })
        });
        let _ = join_all(tasks).await;
    }
}
