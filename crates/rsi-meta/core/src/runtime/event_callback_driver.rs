#![allow(clippy::wildcard_imports)] // This is the event-callback ownership partition.

use super::*;

pub(super) struct EventCallbackDriver<'callback> {
    pub(super) handler: &'callback Arc<dyn EventHandler>,
    pub(super) invocation: InvocationContext,
    pub(super) value: Arc<Value>,
    pub(super) callback_lease: Arc<CallbackLease>,
    pub(super) runtime: &'callback Runtime,
    pub(super) cancellation: &'callback CancellationToken,
    pub(super) deadline: tokio::time::Instant,
}

impl EventCallbackDriver<'_> {
    pub(super) async fn run(self) -> Result<(bool, configuration::OwnedJsonValue)> {
        let Self {
            handler,
            invocation,
            value,
            callback_lease,
            runtime,
            cancellation,
            deadline,
        } = self;
        let operation = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            handler.handle(invocation, value)
        }));
        let Ok(operation) = operation else {
            let Err(payload) = operation else {
                unreachable!();
            };
            drop_catching_unwind(payload);
            callback_lease.close();
            return Err(MetaError::Event("event handler panicked".to_owned()));
        };

        let mut operation = Some(Box::pin(
            std::panic::AssertUnwindSafe(operation).catch_unwind(),
        ));
        let (selected, panic_payload) = tokio::select! {
            biased;
            () = runtime.inner.terminal_cancellation.cancelled() => {
                cancellation.cancel();
                (
                    Err(runtime.ensure_admitting().err().unwrap_or(MetaError::RuntimeShuttingDown)),
                    None,
                )
            },
            () = tokio::time::sleep_until(deadline) => {
                cancellation.cancel();
                (Err(MetaError::Timeout("event dispatch")), None)
            },
            result = operation
                .as_mut()
                .expect("the event-handler future lives through selection")
                .as_mut() => match result {
                    Err(payload) => (
                        Err(MetaError::Event("event handler panicked".to_owned())),
                        Some(payload),
                    ),
                    Ok(Ok(EventOutcome::Continue(value))) => (
                        Ok((false, configuration::OwnedJsonValue::new(value))),
                        None,
                    ),
                    Ok(Ok(EventOutcome::Complete(value))) => (
                        Ok((true, configuration::OwnedJsonValue::new(value))),
                        None,
                    ),
                    Ok(Err(error)) => (
                        Err(dispatch::bound_event_callback_error(
                            error,
                            runtime.inner.limits.payloads.maximum_diagnostic_bytes,
                        )),
                        None,
                    ),
                },
            () = cancellation.cancelled() => (Err(MetaError::Cancelled), None),
        };

        // The outcome is already in iterative owned storage. Keep the callback
        // lease open while every user-controlled future and panic payload is
        // destroyed so a completed future's Drop may still use caller_effect.
        let operation_drop_panicked = drop_catching_unwind(operation.take());
        let payload_drop_panicked = panic_payload.is_some_and(drop_catching_unwind);
        callback_lease.close();
        if operation_drop_panicked || payload_drop_panicked {
            Err(MetaError::Event("event handler panicked".to_owned()))
        } else {
            selected
        }
    }
}
