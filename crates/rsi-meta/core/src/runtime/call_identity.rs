use super::*;

impl Runtime {
    fn next_identity(counter: &AtomicU64, resource: &'static str) -> Result<u64> {
        counter
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |previous| {
                previous.checked_add(1)
            })
            .map(|previous| previous + 1)
            .map_err(|_| MetaError::CapacityExhausted { resource })
    }

    pub(super) fn next_call_id(&self) -> Result<CallId> {
        Self::next_identity(&self.inner.next_call, "call identities").map(CallId)
    }

    pub(super) fn next_fiber_id(&self) -> Result<FiberId> {
        Self::next_identity(&self.inner.next_fiber, "fiber identities").map(FiberId)
    }

    pub(super) fn next_generation_id(&self) -> Result<FiberGeneration> {
        Self::next_identity(&self.inner.next_generation, "generation identities")
            .map(FiberGeneration)
    }

    pub(super) fn next_isolation_id(&self) -> Result<IsolationId> {
        Self::next_identity(&self.inner.next_isolation, "isolation identities").map(IsolationId)
    }

    pub(super) fn next_local_isolation_id(&self) -> Result<LocalIsolationId> {
        Self::next_identity(&self.inner.next_isolation, "isolation identities")
            .map(LocalIsolationId)
    }

    pub(super) fn next_attempt_id(&self) -> Result<u64> {
        Self::next_identity(&self.inner.next_attempt, "preparation attempt identities")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::PreparedActivation;

    #[derive(Debug)]
    struct CountActivation(Arc<AtomicUsize>);

    #[async_trait::async_trait]
    impl PluginFactory for CountActivation {
        fn prepare(&self, desired: &ConfigValue) -> Result<PreparedActivation> {
            Ok(PreparedActivation::new(desired.clone()))
        }

        async fn activate(&self, _: ActivationPlan) -> Result<()> {
            self.0.fetch_add(1, Ordering::AcqRel);
            Ok(())
        }
    }

    #[test]
    fn exhaustion_never_wraps_or_reuses_an_identity() {
        let runtime = Runtime::default();
        runtime
            .inner
            .next_call
            .store(u64::MAX - 1, Ordering::Release);

        assert_eq!(runtime.next_call_id(), Ok(CallId(u64::MAX)));
        let exhausted = Err(MetaError::CapacityExhausted {
            resource: "call identities",
        });
        assert_eq!(runtime.next_call_id(), exhausted);
        assert_eq!(runtime.next_call_id(), exhausted);
        assert_eq!(runtime.inner.next_call.load(Ordering::Acquire), u64::MAX);
    }

    #[test]
    fn every_foundation_identity_fails_closed_at_exhaustion() {
        let runtime = Runtime::default();
        let cases = [
            (&runtime.inner.next_fiber, "fiber identities"),
            (&runtime.inner.next_generation, "generation identities"),
            (&runtime.inner.next_isolation, "isolation identities"),
            (
                &runtime.inner.next_attempt,
                "preparation attempt identities",
            ),
        ];
        for (counter, resource) in cases {
            counter.store(u64::MAX, Ordering::Release);
            assert_eq!(
                Runtime::next_identity(counter, resource),
                Err(MetaError::CapacityExhausted { resource })
            );
            assert_eq!(counter.load(Ordering::Acquire), u64::MAX);
        }
    }

    #[tokio::test]
    async fn activation_lineage_exhaustion_fails_before_plugin_entry() {
        let runtime = Runtime::default();
        runtime.inner.next_call.store(u64::MAX, Ordering::Release);
        let activations = Arc::new(AtomicUsize::new(0));
        let fiber = runtime
            .root()
            .apply(
                crate::plugin::resolved_test_factory(Arc::new(CountActivation(Arc::clone(
                    &activations,
                )))),
                ConfigValue::Null,
            )
            .await
            .expect("the failed Fiber remains manageable");

        assert_eq!(activations.load(Ordering::Acquire), 0);
        assert!(matches!(
            fiber.snapshot().state,
            FiberState::Failed(error) if error.contains("call identities")
        ));
        assert_eq!(runtime.inner.next_call.load(Ordering::Acquire), u64::MAX);

        assert!(fiber.dispose().await.is_clean());
        assert!(runtime.shutdown().await.is_complete());
    }

    #[test]
    fn registry_revision_saturates_without_wrapping() {
        let runtime = Runtime::default();
        let mut state = runtime.inner.state.lock().expect("runtime state poisoned");
        state.revision = u64::MAX;
        state.advance_revision();
        assert_eq!(state.revision, u64::MAX);
    }
}
