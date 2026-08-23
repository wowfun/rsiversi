#![allow(clippy::wildcard_imports)] // This is one implementation partition of runtime.

use super::*;

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
            .requires
            .iter()
            .find(|requirement| &requirement.key == key)
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
                .unwrap_or_else(|| Arc::new(InterceptLayers::empty())),
        })
    }

    #[allow(clippy::too_many_lines)] // Admission, tracing, channels, and the owned task form one seam.
    pub(crate) fn open_service(&self, handle: &ServiceHandle) -> Result<ServiceCall> {
        self.ensure_admitting()?;
        let caller_owner = handle.caller.owner.ok_or_else(|| MetaError::StaleService {
            service: handle.binding.key.clone(),
        })?;
        // Activation and reverse-order cleanup may call already-bound
        // dependencies; generation identity and the provider lease fence them.
        let caller_fiber = self.owner_fiber(caller_owner)?;
        let retiring_consumer = {
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
            matches!(data.state, FiberState::Unloading)
        };
        let admission = Arc::clone(&self.inner.service_call_admission)
            .try_acquire_owned()
            .map_err(|_| MetaError::CapacityExhausted {
                resource: "service calls",
            })?;
        let lease = handle
            .binding
            .lease
            .acquire(retiring_consumer)
            .ok_or_else(|| MetaError::StaleService {
                service: handle.binding.key.clone(),
            })?;
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
        let (requests_tx, requests_rx) = mpsc::channel(self.inner.limits.channel_capacity);
        let response_capacity = self.inner.limits.channel_capacity.checked_add(1).ok_or(
            MetaError::CapacityExhausted {
                resource: "service response channel",
            },
        )?;
        let (responses_tx, responses_rx) = mpsc::channel(response_capacity);
        let terminal_response = responses_tx
            .clone()
            .try_reserve_owned()
            .expect("a fresh response channel has its reserved terminal slot");
        let endpoint = Arc::clone(&handle.binding.endpoint);
        let maximum_frame_bytes = self.inner.limits.maximum_frame_bytes;
        let timeout = self.inner.limits.service_call_timeout;
        let deadline = tokio::time::Instant::now() + timeout;
        let runtime = self.clone();
        let terminal_cancellation = self.inner.terminal_cancellation.clone();
        let task_cancellation = cancellation.clone();
        let provider_channel = crate::ProviderChannel {
            requests: requests_rx,
            responses: responses_tx.clone(),
            cancellation: cancellation.clone(),
            maximum_frame_bytes,
        };
        tokio::spawn(async move {
            let _admission = admission;
            let _lease: LeaseGuard = lease;
            let operation = async {
                tokio::select! {
                    biased;
                    () = terminal_cancellation.cancelled() => {
                        task_cancellation.cancel();
                        runtime.ensure_admitting()
                    }
                    result = endpoint.serve(invocation, provider_channel) => result,
                }
            };
            let error = match tokio::time::timeout_at(deadline, operation).await {
                Ok(Ok(())) => None,
                Ok(Err(error)) => Some(error),
                Err(_) => {
                    task_cancellation.cancel();
                    Some(MetaError::Timeout("service call"))
                }
            };
            if let Some(error) = error {
                terminal_response.send(Err(error));
            }
        });
        Ok(ServiceCall {
            requests: Some(requests_tx),
            responses: responses_rx,
            cancellation,
            maximum_frame_bytes,
        })
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
