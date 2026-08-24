#![allow(clippy::wildcard_imports)] // This is one implementation partition of runtime.

use super::*;
use crate::service::{BufferedFrame, CallLease, LeaseGuard, ResponseMessage};

#[derive(Clone, Copy)]
enum CallTerminationSource {
    RuntimeTerminal,
    Deadline,
    Endpoint,
    Cancellation,
}

struct CallDriver {
    requests: mpsc::Receiver<BufferedFrame>,
    responses: mpsc::Sender<ResponseMessage>,
    terminal: mpsc::OwnedPermit<ResponseMessage>,
    byte_admission: Arc<BufferedByteAdmission>,
    byte_resources: Arc<ResourceLedger>,
    cancellation: CancellationToken,
    deadline: tokio::time::Instant,
    maximum_frame_bytes: usize,
    call_lease: Arc<CallLease>,
    provider_lease: LeaseGuard,
    runtime: Runtime,
}

impl CallDriver {
    async fn run(self, endpoint: Arc<dyn ServiceEndpoint>, invocation: InvocationContext) {
        let Self {
            mut requests,
            responses,
            terminal,
            byte_admission,
            byte_resources,
            cancellation,
            deadline,
            maximum_frame_bytes,
            call_lease,
            provider_lease,
            runtime,
        } = self;
        let terminal_result = {
            let provider_channel = crate::ProviderChannel {
                requests: &mut requests,
                responses: &responses,
                byte_admission: &byte_admission,
                byte_resources: &byte_resources,
                cancellation: &cancellation,
                deadline,
                maximum_frame_bytes,
            };
            let mut operation = Some(Box::pin(
                std::panic::AssertUnwindSafe(endpoint.serve(invocation, provider_channel))
                    .catch_unwind(),
            ));
            let (selected, source) = tokio::select! {
                biased;
                () = runtime.inner.terminal_cancellation.cancelled() => {
                    (
                        Err(runtime.ensure_admitting().err().unwrap_or(MetaError::RuntimeShuttingDown)),
                        CallTerminationSource::RuntimeTerminal,
                    )
                }
                () = tokio::time::sleep_until(deadline) => {
                    (
                        Err(MetaError::Timeout("service call")),
                        CallTerminationSource::Deadline,
                    )
                }
                result = operation
                    .as_mut()
                    .expect("the provider future lives through selection")
                    .as_mut() => (
                        match result {
                            Err(_) => Err(MetaError::ServiceEndpointPanicked),
                            Ok(Ok(())) if cancellation.is_cancelled() => Err(MetaError::Cancelled),
                            Ok(Ok(())) => Ok(()),
                            Ok(Err(error)) => Err(super::dispatch::bound_service_callback_error(
                                error,
                                runtime.inner.limits.payloads.maximum_diagnostic_bytes,
                            )),
                        },
                        CallTerminationSource::Endpoint,
                    ),
                () = cancellation.cancelled() => (
                    Err(MetaError::Cancelled),
                    CallTerminationSource::Cancellation,
                ),
            };
            if drop_catching_unwind(operation.take())
                && matches!(
                    source,
                    CallTerminationSource::Endpoint | CallTerminationSource::Cancellation
                )
            {
                Err(MetaError::ServiceEndpointPanicked)
            } else {
                selected
            }
        };
        // The reserved slot makes publication synchronous. Wake the shared
        // cancellation token only afterwards so a caller's biased receive can
        // preserve this authoritative terminal result.
        terminal.send(ResponseMessage::Terminal(terminal_result));
        drop(provider_lease);
        drop(call_lease);
        cancellation.cancel();
    }
}

impl Runtime {
    pub(super) fn service(&self, context: &Context, key: &ServiceKey) -> Result<ServiceHandle> {
        let owner = context
            .owner
            .ok_or_else(|| MetaError::UndeclaredRequirement {
                service: key.clone(),
            })?;
        let fiber = self.owner_fiber(owner)?;
        let data = fiber.data.lock().expect("fiber state poisoned");
        Self::validate_owner_data(owner, &data, true)?;
        let requirement = data
            .descriptor
            .as_deref()
            .expect("registered Fiber retains its descriptor")
            .requirement(key)
            .ok_or_else(|| MetaError::UndeclaredRequirement {
                service: key.clone(),
            })?;
        let binding = data
            .active
            .as_ref()
            .and_then(|active| active.bindings.get(key))
            .cloned()
            .ok_or_else(|| MetaError::ServiceUnavailable {
                service: key.clone(),
            })?;
        if binding.contract != requirement.contract || binding.version != requirement.version {
            return Err(MetaError::ContractMismatch {
                service: key.clone(),
                expected_id: requirement.contract.clone(),
                expected_version: requirement.version,
                actual_id: binding.contract.clone(),
                actual_version: binding.version,
            });
        }
        Ok(ServiceHandle {
            runtime: self.clone(),
            caller: context.clone(),
            binding,
            overlay: context
                .intercepts
                .get(key)
                .cloned()
                .unwrap_or_else(InterceptLayers::shared_empty),
        })
    }

    #[allow(clippy::too_many_lines)] // Admission, tracing, channels, and the owned task form one seam.
    pub(crate) fn open_service(&self, handle: &ServiceHandle) -> Result<ServiceCall> {
        let caller_owner = handle.caller.owner.ok_or_else(|| MetaError::StaleService {
            service: handle.binding.key.clone(),
        })?;
        // Activation and reverse-order cleanup may call already-bound
        // dependencies; generation identity and the provider lease fence them.
        let caller_fiber = self.owner_fiber(caller_owner)?;
        let executor = caller_fiber.executor.clone();
        let retiring_consumer = Self::validate_service_caller(handle, caller_owner, &caller_fiber)?;
        let runtime_admission = self.begin_admission(retiring_consumer)?;
        let deadline = tokio::time::Instant::now()
            .checked_add(self.inner.limits.deadlines.service_call)
            .expect("validated service-call deadline fits Tokio Instant");
        let admission = Arc::clone(&self.inner.service_call_admission)
            .try_acquire_owned()
            .map_err(|_| {
                self.inner.resources.service_calls.record_rejection();
                MetaError::CapacityExhausted {
                    resource: "service calls",
                }
            })?;
        let call_resource = self.inner.resources.service_calls.try_reserve(1).ok_or(
            MetaError::CapacityExhausted {
                resource: "service calls",
            },
        )?;
        let lease = Self::acquire_service_provider_lease(
            handle,
            caller_owner,
            &caller_fiber,
            retiring_consumer,
        )?;
        let call_id = CallId(self.inner.next_call.fetch_add(1, Ordering::AcqRel) + 1);
        let provider_context = self.provider_context(&handle.binding, &handle.caller, call_id)?;
        let immediate_caller = caller_owner.fiber;
        let origin = handle
            .caller
            .trace
            .as_ref()
            .map_or(immediate_caller, |trace| trace.origin);
        let parent_call = handle
            .caller
            .trace
            .as_ref()
            .and_then(|trace| trace.parent_call);
        let cancellation = CancellationToken::new();
        let invocation = InvocationContext::new(
            call_id,
            parent_call,
            origin,
            immediate_caller,
            handle.binding.provider,
            handle.binding.generation,
            Arc::clone(&handle.overlay),
            handle.caller.clone(),
            provider_context,
            cancellation.clone(),
        );
        let channel_capacity = self.inner.limits.execution.channel_capacity;
        let (requests_tx, requests_rx) = mpsc::channel(channel_capacity);
        let response_capacity =
            channel_capacity
                .checked_add(1)
                .ok_or(MetaError::CapacityExhausted {
                    resource: "service response channel",
                })?;
        let (responses_tx, responses_rx) = mpsc::channel(response_capacity);
        let terminal_response = responses_tx
            .clone()
            .try_reserve_owned()
            .expect("a fresh response channel has its reserved terminal slot");
        let endpoint = Arc::clone(&handle.binding.endpoint);
        let maximum_frame_bytes = self.inner.limits.payloads.maximum_frame_bytes;
        let runtime = self.clone();
        let call_lease = Arc::new(CallLease::new(runtime_admission, admission, call_resource));
        let driver = CallDriver {
            requests: requests_rx,
            responses: responses_tx,
            terminal: terminal_response,
            byte_admission: Arc::clone(&self.inner.service_byte_admission),
            byte_resources: Arc::clone(&self.inner.resources.buffered_service_bytes),
            cancellation: cancellation.clone(),
            deadline,
            maximum_frame_bytes,
            call_lease: Arc::clone(&call_lease),
            provider_lease: lease,
            runtime,
        };
        executor.spawn(driver.run(endpoint, invocation));
        Ok(ServiceCall {
            requests: Some(requests_tx),
            responses: Some(responses_rx),
            byte_admission: Arc::clone(&self.inner.service_byte_admission),
            byte_resources: Arc::clone(&self.inner.resources.buffered_service_bytes),
            cancellation,
            deadline,
            maximum_frame_bytes,
            lease: Some(call_lease),
            terminal_observed: false,
        })
    }

    pub(super) fn validate_service_caller(
        handle: &ServiceHandle,
        caller_owner: Owner,
        caller_fiber: &Arc<Fiber>,
    ) -> Result<bool> {
        let data = caller_fiber.data.lock().expect("fiber state poisoned");
        Self::validate_owner_data(caller_owner, &data, true)?;
        let still_bound = data.active.as_ref().is_some_and(|active| {
            active
                .bindings
                .get(&handle.binding.key)
                .is_some_and(|binding| Arc::ptr_eq(binding, &handle.binding))
        });
        if !still_bound {
            return Err(MetaError::StaleService {
                service: handle.binding.key.clone(),
            });
        }
        Ok(matches!(data.state, FiberState::Unloading))
    }

    pub(super) fn acquire_service_provider_lease(
        handle: &ServiceHandle,
        caller_owner: Owner,
        caller_fiber: &Arc<Fiber>,
        retiring_consumer: bool,
    ) -> Result<LeaseGuard> {
        let lease = handle
            .binding
            .lease
            .acquire(retiring_consumer)
            .ok_or_else(|| MetaError::StaleService {
                service: handle.binding.key.clone(),
            })?;
        // Provider admission protects the endpoint while this final caller
        // check linearizes against reconfiguration and disposal. Any failure
        // drops the provisional provider lease before channel allocation.
        Self::validate_service_caller(handle, caller_owner, caller_fiber)?;
        Ok(lease)
    }

    fn provider_context(
        &self,
        binding: &ProviderBinding,
        caller: &Context,
        call_id: CallId,
    ) -> Result<Context> {
        let provider = {
            let state = self.inner.state.lock().expect("runtime state poisoned");
            state.fibers.get(&binding.provider).cloned()
        }
        .ok_or_else(|| MetaError::StaleService {
            service: binding.key.clone(),
        })?;
        let mut context = provider.context(binding.generation);
        context.trace = Some(CallTrace {
            origin: caller.trace.as_ref().map_or_else(
                || caller.owner.map_or(ROOT_FIBER, |owner| owner.fiber),
                |trace| trace.origin,
            ),
            parent_call: Some(call_id),
        });
        Ok(context)
    }
}
