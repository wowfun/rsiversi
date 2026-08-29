#![allow(clippy::wildcard_imports)] // This is one implementation partition of runtime.

use super::*;
use crate::service::CallLease;

mod driver;
mod endpoint_driver;

use crate::service::MessageChannel;
use driver::CallDriver;

impl Runtime {
    pub(super) fn service(&self, context: &Context, key: &ServiceKey) -> Result<Capability> {
        let owner = context.owner.ok_or_else(|| MetaError::ServiceUnavailable {
            service: key.clone(),
        })?;
        let fiber = self.owner_fiber(owner)?;
        let (binding, capabilities) = {
            let data = fiber.data.lock().expect("fiber state poisoned");
            Self::validate_live_owner_data(owner, &data)?;
            let active = data
                .active
                .as_ref()
                .ok_or_else(|| MetaError::ServiceUnavailable {
                    service: key.clone(),
                })?;
            let slot = ServiceSlot {
                key: key.clone(),
                isolation: Self::isolation_for(&context.isolation, key),
            };
            let binding = if let Some(supply) = active.services.get(&slot) {
                Arc::clone(&supply.binding)
            } else if let Some(binding) = active.bindings.get(key) {
                Arc::clone(binding)
            } else {
                return Err(MetaError::ServiceUnavailable {
                    service: key.clone(),
                });
            };
            (binding, Arc::clone(&active.capabilities))
        };
        self.mint_capability(context, owner, &capabilities, binding)
    }

    #[allow(clippy::too_many_lines)] // Admission, tracing, channels, and the owned task form one seam.
    pub(crate) fn open_service(&self, handle: &Capability) -> Result<CapabilityCall> {
        let caller_owner = handle.holder.owner.ok_or(MetaError::StaleCapability)?;
        let capability_use = handle.entry.acquire_use()?;
        let caller_fiber = self.owner_fiber(caller_owner)?;
        let executor = caller_fiber.executor.clone();
        let caller_admission =
            Self::validate_capability_holder(handle, caller_owner, &caller_fiber)?;
        let caller_lease = caller_admission
            .acquire(false)
            .ok_or(MetaError::StaleCapability)?;
        Self::validate_capability_holder(handle, caller_owner, &caller_fiber)?;
        let runtime_admission = self.begin_admission(false)?;
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
        let lease = handle
            .entry
            .binding
            .lease
            .acquire(false)
            .ok_or(MetaError::StaleCapability)?;
        Self::validate_capability_holder(handle, caller_owner, &caller_fiber)?;
        let call_id = self.next_call_id()?;
        let provider_context =
            self.provider_context(&handle.entry.binding, &handle.holder, call_id)?;
        let provider_message_context = provider_context.clone();
        let immediate_caller = caller_owner.fiber;
        let origin = handle
            .holder
            .trace
            .as_ref()
            .map_or(immediate_caller, |trace| trace.origin);
        let lineage_call = handle
            .holder
            .trace
            .as_ref()
            .map_or(call_id, |trace| trace.lineage_call);
        let parent_call = handle
            .holder
            .trace
            .as_ref()
            .and_then(|trace| trace.parent_call);
        let cancellation = CancellationToken::new();
        let invocation = InvocationContext::new(
            call_id,
            lineage_call,
            parent_call,
            origin,
            immediate_caller,
            handle.entry.binding.provider,
            handle.entry.binding.generation,
            handle.holder.clone(),
            provider_context,
            cancellation.clone(),
        );
        let channel_capacity = self.inner.limits.execution.channel_capacity;
        let (requests_tx, requests_rx) = mpsc::channel(channel_capacity);
        let request_channel = MessageChannel::new(channel_capacity);
        let response_capacity =
            channel_capacity
                .checked_add(1)
                .ok_or(MetaError::CapacityExhausted {
                    resource: "service response channel",
                })?;
        let (responses_tx, responses_rx) = mpsc::channel(response_capacity);
        let response_channel = MessageChannel::new(channel_capacity);
        let terminal_response = responses_tx
            .clone()
            .try_reserve_owned()
            .expect("a fresh response channel has its reserved terminal slot");
        let endpoint = handle
            .entry
            .binding
            .endpoint
            .lock()
            .expect("service endpoint state poisoned")
            .clone()
            .ok_or(MetaError::StaleCapability)?;
        let maximum_message_bytes = self.inner.limits.payloads.maximum_message_bytes;
        let maximum_capabilities_per_message =
            self.inner.limits.topology.maximum_capabilities_per_message;
        let runtime = self.clone();
        let call_lease = Arc::new(CallLease::new(
            runtime_admission,
            caller_lease,
            admission,
            call_resource,
        ));
        let driver = CallDriver {
            provider_context: provider_message_context,
            requests: requests_rx,
            responses: responses_tx,
            terminal: terminal_response,
            message_admission: Arc::clone(&self.inner.message_admission),
            response_channel,
            byte_resources: Arc::clone(&self.inner.resources.buffered_message_bytes),
            capability_resources: Arc::clone(&self.inner.resources.queued_capability_references),
            cancellation: cancellation.clone(),
            deadline,
            maximum_message_bytes,
            maximum_capabilities_per_message,
            call_lease: Arc::clone(&call_lease),
            capability_use,
            provider_lease: lease,
            runtime,
        };
        executor.spawn(driver.run(endpoint, invocation));
        Ok(CapabilityCall {
            context: handle.holder.clone(),
            requests: Some(requests_tx),
            responses: Some(responses_rx),
            message_admission: Arc::clone(&self.inner.message_admission),
            request_channel,
            byte_resources: Arc::clone(&self.inner.resources.buffered_message_bytes),
            capability_resources: Arc::clone(&self.inner.resources.queued_capability_references),
            cancellation,
            deadline,
            maximum_message_bytes,
            maximum_capabilities_per_message,
            lease: Some(call_lease),
            terminal_result: None,
        })
    }

    pub(super) fn validate_capability_holder(
        handle: &Capability,
        caller_owner: Owner,
        caller_fiber: &Arc<Fiber>,
    ) -> Result<Arc<AdmissionLease>> {
        let data = caller_fiber.data.lock().expect("fiber state poisoned");
        Self::validate_live_owner_data(caller_owner, &data)?;
        let active = data.active.as_ref().ok_or(MetaError::StaleCapability)?;
        if handle.holder.owner != Some(caller_owner) {
            return Err(MetaError::StaleCapability);
        }
        Ok(Arc::clone(&active.lease))
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
            lineage_call: caller
                .trace
                .as_ref()
                .map_or(call_id, |trace| trace.lineage_call),
            parent_call: Some(call_id),
        });
        Ok(context)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Message, PreparedActivation, ProviderChannel, Requirement};

    const V1: ContractVersion = ContractVersion(1);
    const CONTRACT: &str = "test.lineage";

    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    struct ObservedCall {
        hop: &'static str,
        call: CallId,
        lineage: CallId,
        parent: Option<CallId>,
        origin: FiberId,
    }

    #[derive(Debug)]
    struct ChainEndpoint {
        hop: &'static str,
        next: Option<&'static str>,
        observed: Arc<Mutex<Vec<ObservedCall>>>,
    }

    #[async_trait::async_trait]
    impl ServiceEndpoint for ChainEndpoint {
        async fn serve(
            &self,
            invocation: InvocationContext,
            mut channel: ProviderChannel<'_>,
        ) -> Result<()> {
            self.observed
                .lock()
                .expect("lineage observations poisoned")
                .push(ObservedCall {
                    hop: self.hop,
                    call: invocation.call_id(),
                    lineage: invocation.lineage_call_id(),
                    parent: invocation.parent_call_id(),
                    origin: invocation.origin(),
                });
            while let Some(message) = channel.recv().await {
                let response = if let Some(next) = self.next {
                    invocation
                        .provider_context()
                        .service(next)?
                        .invoke(message)
                        .await?
                } else {
                    message
                };
                channel.send(response).await?;
            }
            Ok(())
        }
    }

    #[derive(Debug)]
    struct ChainFactory {
        hop: &'static str,
        service: &'static str,
        next: Option<&'static str>,
        observed: Arc<Mutex<Vec<ObservedCall>>>,
    }

    #[async_trait::async_trait]
    impl PluginFactory for ChainFactory {
        fn prepare(&self, desired: &ConfigValue) -> Result<PreparedActivation> {
            let prepared = PreparedActivation::new(desired.clone());
            Ok(if let Some(next) = self.next {
                prepared.requiring(Requirement::new(next, CONTRACT, V1))
            } else {
                prepared
            })
        }

        async fn activate(&self, plan: ActivationPlan) -> Result<()> {
            plan.context().provide(
                self.service,
                CONTRACT,
                V1,
                Arc::new(ChainEndpoint {
                    hop: self.hop,
                    next: self.next,
                    observed: Arc::clone(&self.observed),
                }),
            )?;
            Ok(())
        }
    }

    #[derive(Debug)]
    struct CaptureFactory(Arc<Mutex<Option<(Capability, CallId)>>>);

    #[async_trait::async_trait]
    impl PluginFactory for CaptureFactory {
        fn prepare(&self, desired: &ConfigValue) -> Result<PreparedActivation> {
            Ok(PreparedActivation::new(desired.clone())
                .requiring(Requirement::new("a-head", CONTRACT, V1)))
        }

        async fn activate(&self, plan: ActivationPlan) -> Result<()> {
            *self.0.lock().expect("captured capability poisoned") = plan
                .inject("a-head")
                .cloned()
                .map(|capability| (capability, plan.lineage_call_id()));
            Ok(())
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn activation_to_b_to_a_preserves_seed_and_immediate_parents() {
        let runtime = Runtime::default();
        let observed = Arc::new(Mutex::new(Vec::new()));
        let mut fibers = Vec::new();
        for (hop, service, next) in [
            ("a-tail", "a-tail", None),
            ("b", "b", Some("a-tail")),
            ("a-head", "a-head", Some("b")),
        ] {
            fibers.push(
                runtime
                    .root()
                    .apply(
                        crate::plugin::resolved_test_factory(Arc::new(ChainFactory {
                            hop,
                            service,
                            next,
                            observed: Arc::clone(&observed),
                        })),
                        ConfigValue::Null,
                    )
                    .await
                    .expect("chain provider activates"),
            );
        }
        let captured = Arc::new(Mutex::new(None));
        fibers.push(
            runtime
                .root()
                .apply(
                    crate::plugin::resolved_test_factory(Arc::new(CaptureFactory(Arc::clone(
                        &captured,
                    )))),
                    ConfigValue::Null,
                )
                .await
                .expect("chain client activates"),
        );
        let client = fibers.last().expect("client Fiber is retained").id();
        let (capability, lineage) = captured
            .lock()
            .expect("captured capability poisoned")
            .clone()
            .expect("activation receives a-head");
        assert_ne!(lineage, CallId(0));

        let call =
            tokio::spawn(async move { capability.invoke(Message::new(b"lineage".to_vec())).await });
        assert_eq!(
            call.await
                .expect("call task succeeds")
                .expect("nested chain succeeds")
                .as_bytes(),
            b"lineage"
        );
        let observed = observed
            .lock()
            .expect("lineage observations poisoned")
            .clone();
        assert_eq!(observed.len(), 3);
        assert_eq!(
            observed.iter().map(|item| item.hop).collect::<Vec<_>>(),
            ["a-head", "b", "a-tail"]
        );
        assert_eq!(observed[0].lineage, lineage);
        assert_ne!(observed[0].call, lineage);
        assert_eq!(observed[0].parent, None);
        assert!(observed.iter().all(|item| item.origin == client));
        assert_eq!(observed[1].lineage, lineage);
        assert_eq!(observed[1].parent, Some(observed[0].call));
        assert_eq!(observed[2].lineage, lineage);
        assert_eq!(observed[2].parent, Some(observed[1].call));
        assert_ne!(observed[1].call, observed[0].call);
        assert_ne!(observed[2].call, observed[1].call);
        drop(captured);
        for fiber in fibers.into_iter().rev() {
            assert!(fiber.dispose().await.is_clean());
        }
        assert!(runtime.shutdown().await.is_complete());
    }
}
