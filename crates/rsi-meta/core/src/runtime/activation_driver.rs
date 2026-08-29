#![allow(clippy::wildcard_imports)] // This is the activation ownership partition.

use super::*;

type ActivationResult = std::result::Result<Result<()>, Box<dyn std::any::Any + Send>>;

struct ActivationOperationGuard<'activation> {
    operation: Option<BoxFuture<'activation, ActivationResult>>,
}

impl ActivationOperationGuard<'_> {
    fn destroy(&mut self) -> bool {
        drop_catching_unwind(self.operation.take())
    }
}

impl Drop for ActivationOperationGuard<'_> {
    fn drop(&mut self) {
        // If the driver itself is cancelled while the plugin is Pending, the
        // future still crosses the same destructor-panic containment seam.
        self.destroy();
    }
}

pub(super) struct ActivationDriver<'activation> {
    pub(super) factory: &'activation RetainedFactory,
    pub(super) plan: ActivationPlan,
    pub(super) apply_cancellation: &'activation CancellationToken,
    pub(super) generation_cancellation: &'activation CancellationToken,
    pub(super) deadline: tokio::time::Instant,
}

impl ActivationDriver<'_> {
    pub(super) async fn run(self) -> Result<()> {
        let Self {
            factory,
            plan,
            apply_cancellation,
            generation_cancellation,
            deadline,
        } = self;
        let operation =
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| factory.activate(plan)));
        let Ok(operation) = operation else {
            let Err(payload) = operation else {
                unreachable!();
            };
            drop_catching_unwind(payload);
            return Err(MetaError::Activation(
                "plugin activation panicked".to_owned(),
            ));
        };
        let mut operation = ActivationOperationGuard {
            operation: Some(
                std::panic::AssertUnwindSafe(operation)
                    .catch_unwind()
                    .boxed(),
            ),
        };
        let (selected, panic_payload) = tokio::select! {
            biased;
            () = apply_cancellation.cancelled() => (Err(MetaError::Cancelled), None),
            () = generation_cancellation.cancelled() => (Err(MetaError::Cancelled), None),
            () = tokio::time::sleep_until(deadline) => (
                Err(MetaError::Timeout("plugin activation")),
                None,
            ),
            result = operation
                .operation
                .as_mut()
                .expect("the activation future lives through selection") => match result {
                    Ok(result) => (result, None),
                    Err(payload) => (
                        Err(MetaError::Activation("plugin activation panicked".to_owned())),
                        Some(payload),
                    ),
                },
        };
        let operation_drop_panicked = operation.destroy();
        let payload_drop_panicked = panic_payload.is_some_and(drop_catching_unwind);
        if operation_drop_panicked || payload_drop_panicked {
            Err(MetaError::Activation(
                "plugin activation teardown panicked".to_owned(),
            ))
        } else {
            selected
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::PreparedActivation;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::sync::Notify;

    struct RecursivePanicPayload;

    impl Drop for RecursivePanicPayload {
        fn drop(&mut self) {
            std::panic::panic_any(Self);
        }
    }

    struct PanickingFutureDrop;

    impl Drop for PanickingFutureDrop {
        fn drop(&mut self) {
            std::panic::panic_any(RecursivePanicPayload);
        }
    }

    struct CountState(Arc<AtomicUsize>);

    impl Drop for CountState {
        fn drop(&mut self) {
            self.0.fetch_add(1, Ordering::AcqRel);
        }
    }

    struct PendingFactory {
        entered: Arc<Notify>,
    }

    impl fmt::Debug for PendingFactory {
        fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter.debug_struct("PendingFactory").finish()
        }
    }

    #[async_trait::async_trait]
    impl PluginFactory for PendingFactory {
        fn prepare(&self, desired: &ConfigValue) -> Result<PreparedActivation> {
            Ok(PreparedActivation::new(desired.clone()))
        }

        async fn activate(&self, _plan: ActivationPlan) -> Result<()> {
            let _drop = PanickingFutureDrop;
            self.entered.notify_one();
            std::future::pending().await
        }
    }

    #[tokio::test]
    async fn cancellation_destroys_future_and_state_inside_containment() {
        let entered = Arc::new(Notify::new());
        let drops = Arc::new(AtomicUsize::new(0));
        let factory = RetainedFactory::new(Arc::new(PendingFactory {
            entered: Arc::clone(&entered),
        }));
        let runtime = Runtime::default();
        let mut context = runtime.root();
        context.install_activation_lineage(FiberId(1), CallId(1));
        let plan = ActivationPlan::new(
            context,
            Arc::new(ConfigValue::Null),
            BTreeMap::new(),
            BTreeMap::new(),
            Some(crate::plugin::PreparedState::new_for_test(CountState(
                Arc::clone(&drops),
            ))),
        );
        let apply_cancellation = CancellationToken::new();
        let generation_cancellation = CancellationToken::new();
        let cancel = generation_cancellation.clone();
        let cancelling = async {
            entered.notified().await;
            cancel.cancel();
        };
        let driver = ActivationDriver {
            factory: &factory,
            plan,
            apply_cancellation: &apply_cancellation,
            generation_cancellation: &generation_cancellation,
            deadline: tokio::time::Instant::now() + std::time::Duration::from_secs(5),
        };

        let (result, ()) = tokio::join!(driver.run(), cancelling);
        assert!(matches!(result, Err(MetaError::Activation(_))));
        assert_eq!(drops.load(Ordering::Acquire), 1);
    }
}
