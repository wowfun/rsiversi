#![allow(clippy::wildcard_imports)] // This is the provider-operation ownership partition.

use super::super::*;

#[derive(Clone, Copy)]
pub(super) enum CallTerminationSource {
    RuntimeTerminal,
    Deadline,
    Endpoint,
    Cancellation,
}

pub(super) struct EndpointOutcome {
    pub(super) selected: Result<()>,
    pub(super) source: CallTerminationSource,
    pub(super) cleanup_panicked: bool,
}

pub(super) struct EndpointDriver<'call> {
    pub(super) endpoint: &'call Arc<dyn ServiceEndpoint>,
    pub(super) invocation: InvocationContext,
    pub(super) channel: crate::ProviderChannel<'call>,
    pub(super) callback_lease: Arc<CallbackLease>,
    pub(super) runtime: &'call Runtime,
    pub(super) cancellation: &'call CancellationToken,
    pub(super) deadline: tokio::time::Instant,
}

impl EndpointDriver<'_> {
    pub(super) async fn run(self) -> EndpointOutcome {
        let Self {
            endpoint,
            invocation,
            channel,
            callback_lease,
            runtime,
            cancellation,
            deadline,
        } = self;
        let operation = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            endpoint.serve(invocation, channel)
        }));
        let Ok(operation) = operation else {
            let Err(payload) = operation else {
                unreachable!();
            };
            let payload_drop_panicked = drop_catching_unwind(payload);
            callback_lease.close();
            return EndpointOutcome {
                selected: Err(MetaError::ServiceEndpointPanicked),
                source: CallTerminationSource::Endpoint,
                cleanup_panicked: payload_drop_panicked,
            };
        };
        let mut operation = Some(Box::pin(
            std::panic::AssertUnwindSafe(operation).catch_unwind(),
        ));
        let (selected, source, panic_payload) = tokio::select! {
            biased;
            () = runtime.inner.terminal_cancellation.cancelled() => (
                Err(runtime.ensure_admitting().err().unwrap_or(MetaError::RuntimeShuttingDown)),
                CallTerminationSource::RuntimeTerminal,
                None,
            ),
            () = tokio::time::sleep_until(deadline) => (
                Err(MetaError::Timeout("service call")),
                CallTerminationSource::Deadline,
                None,
            ),
            result = operation
                .as_mut()
                .expect("the provider future lives through selection")
                .as_mut() => match result {
                    Err(payload) => (
                        Err(MetaError::ServiceEndpointPanicked),
                        CallTerminationSource::Endpoint,
                        Some(payload),
                    ),
                    Ok(Ok(())) if cancellation.is_cancelled() => (
                        Err(MetaError::Cancelled),
                        CallTerminationSource::Endpoint,
                        None,
                    ),
                    Ok(Ok(())) => (Ok(()), CallTerminationSource::Endpoint, None),
                    Ok(Err(error)) => (
                        Err(super::super::diagnostics::bound_service_error(
                            error,
                            runtime.inner.limits.payloads.maximum_diagnostic_bytes,
                        )),
                        CallTerminationSource::Endpoint,
                        None,
                    ),
                },
            () = cancellation.cancelled() => (
                Err(MetaError::Cancelled),
                CallTerminationSource::Cancellation,
                None,
            ),
        };
        let operation_drop_panicked = drop_catching_unwind(operation.take());
        let payload_drop_panicked = panic_payload.is_some_and(drop_catching_unwind);
        callback_lease.close();
        EndpointOutcome {
            selected,
            source,
            cleanup_panicked: operation_drop_panicked || payload_drop_panicked,
        }
    }
}
