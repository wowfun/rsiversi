#![allow(clippy::wildcard_imports)] // This is one implementation partition of runtime.

use super::*;
use crate::service::LeaseGuard;

struct DispatchOwnership {
    _runtime_admission: LeaseGuard,
    _dispatch: ResourceReservation,
}

struct DispatchListener {
    runtime: Runtime,
    binding: Option<Arc<ListenerBinding>>,
    ownership: EventOwnership,
}

impl DispatchListener {
    fn new(runtime: &Runtime, binding: Arc<ListenerBinding>) -> Self {
        Self {
            runtime: runtime.clone(),
            ownership: binding.ownership.clone(),
            binding: Some(binding),
        }
    }
}

impl std::ops::Deref for DispatchListener {
    type Target = ListenerBinding;

    fn deref(&self) -> &Self::Target {
        self.binding
            .as_deref()
            .expect("a live dispatch snapshot owns its listener binding")
    }
}

impl Drop for DispatchListener {
    fn drop(&mut self) {
        if self.binding.take().is_some_and(drop_catching_unwind) {
            self.ownership
                .retain_destructor_failure("event listener destructor panicked");
            self.runtime
                .mark_terminal_owned("event listener destruction panicked");
        }
    }
}

impl Runtime {
    #[allow(clippy::too_many_lines)] // Four modes share one snapshot and callback-admission seam.
    pub(super) async fn dispatch_event(
        &self,
        context: &Context,
        event: &EventKey,
        mode: DispatchMode,
        value: configuration::OwnedJsonValue,
        target: Option<Arc<dyn EventTarget>>,
    ) -> Result<EventReceipt> {
        let runtime_admission = self.begin_admission(false)?;
        let dispatch = self.inner.resources.event_dispatches.try_reserve(1).ok_or(
            MetaError::CapacityExhausted {
                resource: "event dispatches",
            },
        )?;
        let ownership = DispatchOwnership {
            _runtime_admission: runtime_admission,
            _dispatch: dispatch,
        };
        let deadline = tokio::time::Instant::now()
            .checked_add(self.inner.limits.deadlines.event_dispatch)
            .ok_or_else(|| {
                MetaError::InvalidInput("event dispatch deadline overflow".to_owned())
            })?;
        if let Some(owner) = context.owner {
            let fiber = self.owner_fiber(owner)?;
            let data = fiber.data.lock().expect("fiber state poisoned");
            Self::validate_live_owner_data(owner, &data)?;
        }
        let (value, _) = configuration::validate_owned_json_payload(
            value,
            &self.inner.limits.payloads,
            self.inner.limits.payloads.maximum_message_bytes,
        )?;
        let value = Arc::new(value.into_inner());
        let candidates = {
            let state = self.inner.state.lock().expect("runtime state poisoned");
            state
                .listeners
                .get(event)
                .map_or_else(Vec::new, ListenerRegistry::snapshot)
        }
        .into_iter()
        .map(|binding| DispatchListener::new(self, binding))
        .collect();
        let (listeners, _ownership) = self
            .select_event_listeners(candidates, target, deadline, ownership)
            .await?;
        let caller = context.owner.map_or(ROOT_FIBER, |owner| owner.fiber);
        match mode {
            DispatchMode::Parallel => {
                let concurrency = self
                    .inner
                    .limits
                    .execution
                    .maximum_concurrent_event_callbacks;
                let mut results = futures_util::stream::iter(listeners)
                    .map(|listener| {
                        let value = Arc::clone(&value);
                        async move {
                            self.invoke_listener(context, caller, listener, value, deadline)
                                .await
                        }
                    })
                    .buffer_unordered(concurrency);
                let mut invoked = 0;
                let mut errors = EventDiagnostics::default();
                while let Some(result) = results.next().await {
                    match result {
                        Ok(Some(_)) => invoked += 1,
                        Ok(None) => {}
                        Err(error) => {
                            let message = bound_formatted_diagnostic(
                                format_args!("{error}"),
                                self.inner.limits.payloads.maximum_diagnostic_bytes,
                            );
                            errors.push(
                                &message,
                                self.inner.limits.payloads.maximum_diagnostic_entries,
                                self.inner.limits.payloads.maximum_diagnostic_bytes,
                            );
                        }
                    }
                }
                if let Some(message) =
                    errors.finish(self.inner.limits.payloads.maximum_diagnostic_bytes)
                {
                    return Err(MetaError::Event(message));
                }
                Ok(EventReceipt {
                    invoked,
                    completed: None,
                })
            }
            DispatchMode::Waterfall => {
                let mut current = value;
                let mut invoked = 0;
                for listener in listeners {
                    let Some(outcome) = self
                        .invoke_listener(context, caller, listener, Arc::clone(&current), deadline)
                        .await?
                    else {
                        continue;
                    };
                    invoked += 1;
                    match outcome {
                        EventOutcome::Continue(next) => current = Arc::new(next),
                        EventOutcome::Complete(done) => {
                            return Ok(EventReceipt {
                                invoked,
                                completed: Some(done),
                            });
                        }
                    }
                }
                Ok(EventReceipt {
                    invoked,
                    completed: Some(
                        Arc::try_unwrap(current).unwrap_or_else(|value| (*value).clone()),
                    ),
                })
            }
            DispatchMode::Serial => {
                let mut invoked = 0;
                for listener in listeners {
                    let Some(outcome) = self
                        .invoke_listener(context, caller, listener, Arc::clone(&value), deadline)
                        .await?
                    else {
                        continue;
                    };
                    invoked += 1;
                    match outcome {
                        EventOutcome::Continue(_) => {}
                        EventOutcome::Complete(done) => {
                            return Ok(EventReceipt {
                                invoked,
                                completed: Some(done),
                            });
                        }
                    }
                }
                Ok(EventReceipt {
                    invoked,
                    completed: None,
                })
            }
            DispatchMode::Emit => {
                let mut invoked = 0;
                for listener in listeners {
                    if self
                        .invoke_listener(context, caller, listener, Arc::clone(&value), deadline)
                        .await?
                        .is_some()
                    {
                        invoked += 1;
                    }
                }
                Ok(EventReceipt {
                    invoked,
                    completed: None,
                })
            }
        }
    }

    async fn select_event_listeners(
        &self,
        candidates: Vec<DispatchListener>,
        target: Option<Arc<dyn EventTarget>>,
        deadline: tokio::time::Instant,
        ownership: DispatchOwnership,
    ) -> Result<(Vec<DispatchListener>, DispatchOwnership)> {
        let Some(target) = target else {
            if tokio::time::Instant::now() >= deadline {
                return Err(MetaError::Timeout("event dispatch"));
            }
            return Ok((candidates, ownership));
        };
        let maximum_diagnostic_bytes = self.inner.limits.payloads.maximum_diagnostic_bytes;
        let mut selection = tokio::task::spawn_blocking(move || {
            let selected = Self::select_event_listeners_blocking(
                candidates,
                target.as_ref(),
                deadline,
                maximum_diagnostic_bytes,
            );
            (ownership, selected)
        });
        let terminal_cancellation = self.inner.terminal_cancellation.clone();
        let deadline_sleep = tokio::time::sleep_until(deadline);
        tokio::pin!(deadline_sleep);
        let completed = tokio::select! {
            biased;
            () = &mut deadline_sleep => {
                return Err(MetaError::Timeout("event dispatch"));
            }
            completed = &mut selection => completed,
            () = terminal_cancellation.cancelled() => {
                return Err(self.terminal_error());
            }
        };
        match completed {
            Ok((ownership, selected)) => selected.map(|selected| (selected, ownership)),
            Err(error) if error.is_panic() => {
                let message = if drop_catching_unwind(error.into_panic()) {
                    "event target selection worker panic payload destruction panicked"
                } else {
                    "event target selection worker panicked"
                };
                Err(MetaError::Event(message.to_owned()))
            }
            Err(_) => Err(MetaError::Event(
                "event target selection worker was cancelled".to_owned(),
            )),
        }
    }

    fn select_event_listeners_blocking(
        candidates: Vec<DispatchListener>,
        target: &dyn EventTarget,
        deadline: tokio::time::Instant,
        maximum_diagnostic_bytes: usize,
    ) -> Result<Vec<DispatchListener>> {
        let mut selected = Vec::with_capacity(candidates.len());
        for listener in candidates {
            if tokio::time::Instant::now() >= deadline {
                return Err(MetaError::Timeout("event dispatch"));
            }
            if listener.options.global {
                selected.push(listener);
                continue;
            }
            let view = ListenerView::new(
                listener.owner,
                listener.generation,
                Arc::clone(&listener.scope.extensions),
            );
            let selection =
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| target.select(&view)));
            if tokio::time::Instant::now() >= deadline {
                return Err(MetaError::Timeout("event dispatch"));
            }
            let selection = match selection {
                Ok(selection) => selection,
                Err(payload) => {
                    let message = if drop_catching_unwind(payload) {
                        "event target panic payload destruction panicked"
                    } else {
                        "event target panicked"
                    };
                    return Err(MetaError::Event(message.to_owned()));
                }
            };
            let matches = selection.map_err(|error| {
                MetaError::Event(bound_formatted_diagnostic(
                    format_args!("event target failed: {error}"),
                    maximum_diagnostic_bytes,
                ))
            })?;
            if matches {
                selected.push(listener);
            }
        }
        Ok(selected)
    }

    pub(super) async fn withdraw_listener_owned(&self, owner: Owner, id: EventListenerId) -> bool {
        let ownership = {
            let state = self.inner.state.lock().expect("runtime state poisoned");
            let Some(event) = state.listener_events.get(&id) else {
                return false;
            };
            let Some(listener) = state
                .listeners
                .get(event)
                .and_then(|listeners| listeners.get(id))
            else {
                return false;
            };
            if listener.owner != owner.fiber || listener.generation != owner.generation {
                return false;
            }
            listener.ownership.clone()
        };
        ownership.withdraw_for_retirement().await
    }

    /// Removes one exact listener and every reverse ownership reference under
    /// one Runtime transaction. Only [`EventRemoval`] calls this after winning
    /// its one-shot claim.
    pub(super) fn remove_listener_entry(&self, owner: Owner, id: EventListenerId) -> bool {
        let mut state = self.inner.state.lock().expect("runtime state poisoned");
        let Some(event) = state.listener_events.get(&id).cloned() else {
            return false;
        };
        let listener = state
            .listeners
            .get(&event)
            .and_then(|listeners| listeners.get(id))
            .cloned();
        let Some(listener) = listener else {
            return false;
        };
        if listener.owner != owner.fiber || listener.generation != owner.generation {
            return false;
        }
        let Some(fiber) = state.fibers.get(&owner.fiber).cloned() else {
            return false;
        };
        let mut data = fiber.data.lock().expect("fiber state poisoned");
        if data.generation != owner.generation {
            return false;
        }
        let Some(active) = data.active.as_mut() else {
            return false;
        };
        if active.generation != owner.generation || !active.listeners.contains_key(&id) {
            return false;
        }

        let (removed_listener, remove_bucket) = {
            let listeners = state
                .listeners
                .get_mut(&event)
                .expect("listener event has a registry");
            let removed = listeners
                .remove(id)
                .expect("validated listener remains in its registry");
            (removed, listeners.is_empty())
        };
        if remove_bucket {
            state.listeners.remove(&event);
        }
        active.listeners.remove(&id);
        state.listener_events.remove(&id);
        state.advance_revision();
        drop(data);
        drop(state);
        drop(removed_listener);
        drop(listener);
        true
    }

    /// Atomically consumes a once-listener immediately before invocation.
    /// Non-once listeners retain snapshot semantics.
    async fn claim_listener(&self, listener: &ListenerBinding) -> bool {
        !listener.options.once || listener.ownership.claim_once().await
    }

    fn invoke_listener<'a>(
        &'a self,
        caller_context: &'a Context,
        caller: FiberId,
        listener: DispatchListener,
        value: Arc<Value>,
        deadline: tokio::time::Instant,
    ) -> BoxFuture<'a, Result<Option<EventOutcome>>> {
        Box::pin(async move {
            let callback_admission = Arc::clone(&self.inner.event_callback_admission);
            let terminal_cancellation = self.inner.terminal_cancellation.clone();
            let deadline_sleep = tokio::time::sleep_until(deadline);
            tokio::pin!(deadline_sleep);
            let _callback = tokio::select! {
                biased;
                () = terminal_cancellation.cancelled() => {
                    return Err(self.terminal_error());
                }
                () = &mut deadline_sleep => {
                    self.inner.resources.event_callbacks.record_rejection();
                    return Err(MetaError::Timeout("event dispatch"));
                }
                permit = callback_admission.acquire_owned() => {
                    permit.expect("the Runtime never closes callback admission")
                }
            };
            let _callback_usage = self
                .inner
                .resources
                .event_callbacks
                .try_reserve(1)
                .expect("callback semaphore and resource ledger stay synchronized");
            let Some(_lease) = listener.lease.acquire(false) else {
                return Ok(None);
            };
            let claimed = tokio::select! {
                biased;
                () = terminal_cancellation.cancelled() => {
                    return Err(self.terminal_error());
                }
                () = &mut deadline_sleep => {
                    return Err(MetaError::Timeout("event dispatch"));
                }
                claimed = self.claim_listener(&listener) => claimed,
            };
            if !claimed {
                return Ok(None);
            }

            let (invocation, cancellation) =
                self.event_invocation(caller_context, caller, &listener)?;
            let callback_lease = invocation.callback_lease();
            let (complete, value) = event_callback_driver::EventCallbackDriver {
                handler: &listener.handler,
                invocation,
                value,
                callback_lease,
                runtime: self,
                cancellation: &cancellation,
                deadline,
            }
            .run()
            .await?;
            Ok(Some(self.validate_event_outcome(complete, value)?))
        })
    }

    fn event_invocation(
        &self,
        caller_context: &Context,
        caller: FiberId,
        listener: &ListenerBinding,
    ) -> Result<(InvocationContext, CancellationToken)> {
        let call_id = self.next_call_id()?;
        let cancellation = CancellationToken::new();
        let origin = caller_context
            .trace
            .as_ref()
            .map_or(caller, |trace| trace.origin);
        let lineage_call = caller_context
            .trace
            .as_ref()
            .map_or(call_id, |trace| trace.lineage_call);
        let parent_call = caller_context
            .trace
            .as_ref()
            .and_then(|trace| trace.parent_call);
        let provider_context = Context {
            runtime: self.clone(),
            owner: Some(Owner {
                fiber: listener.owner,
                generation: listener.generation,
            }),
            setup_effect: None,
            isolation: Arc::clone(&listener.scope.isolation),
            intercepts: Arc::clone(&listener.scope.intercepts),
            extensions: Arc::clone(&listener.scope.extensions),
            entries: listener.scope.entries,
            encoded_bytes: listener.scope.encoded_bytes,
            trace: Some(CallTrace {
                origin,
                lineage_call,
                parent_call: Some(call_id),
            }),
        };
        Ok((
            InvocationContext::new(
                call_id,
                lineage_call,
                parent_call,
                origin,
                caller,
                listener.owner,
                listener.generation,
                InterceptLayers::shared_empty(),
                caller_context.clone(),
                provider_context,
                cancellation.clone(),
            ),
            cancellation,
        ))
    }

    fn validate_event_outcome(
        &self,
        complete: bool,
        value: configuration::OwnedJsonValue,
    ) -> Result<EventOutcome> {
        let (value, _) = configuration::validate_owned_json_payload(
            value,
            &self.inner.limits.payloads,
            self.inner.limits.payloads.maximum_message_bytes,
        )?;
        let value = value.into_inner();
        Ok(if complete {
            EventOutcome::Complete(value)
        } else {
            EventOutcome::Continue(value)
        })
    }

    fn terminal_error(&self) -> MetaError {
        self.ensure_admitting()
            .err()
            .unwrap_or(MetaError::RuntimeShuttingDown)
    }
}

#[derive(Default)]
struct EventDiagnostics {
    entries: Vec<String>,
    total: usize,
    bytes: usize,
    truncated: bool,
}

impl EventDiagnostics {
    fn push(&mut self, message: &str, maximum_entries: usize, maximum_bytes: usize) {
        self.total = self.total.saturating_add(1);
        if self.entries.len() >= maximum_entries {
            return;
        }
        let separator = usize::from(!self.entries.is_empty()) * 2;
        let Some(remaining) = maximum_bytes
            .checked_sub(self.bytes)
            .and_then(|remaining| remaining.checked_sub(separator))
        else {
            return;
        };
        if remaining == 0 {
            return;
        }
        let retained = truncate_utf8(message, remaining);
        self.truncated |= retained.len() != message.len();
        self.bytes += separator + retained.len();
        self.entries.push(retained.to_owned());
    }

    fn finish(self, maximum_bytes: usize) -> Option<String> {
        if self.total == 0 {
            return None;
        }
        let retained = self.entries.len();
        let omitted = self.total.saturating_sub(retained);
        let mut message = self.entries.join("; ");
        if omitted == 0 && !self.truncated {
            return Some(message);
        }
        let suffix = if omitted == 0 {
            "event diagnostics truncated".to_owned()
        } else {
            format!("{omitted} event errors omitted")
        };
        if suffix.len() >= maximum_bytes {
            return Some(truncate_utf8(&suffix, maximum_bytes).to_owned());
        }
        let separator = if message.is_empty() { "" } else { "; " };
        let Some(available) = maximum_bytes
            .checked_sub(suffix.len())
            .and_then(|remaining| remaining.checked_sub(separator.len()))
        else {
            return Some(suffix);
        };
        message.truncate(truncate_utf8(&message, available).len());
        if message.is_empty() {
            return Some(suffix);
        }
        Some(format!("{message}{separator}{suffix}"))
    }
}

fn truncate_utf8(value: &str, maximum: usize) -> &str {
    let mut end = value.len().min(maximum);
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    &value[..end]
}

const DIAGNOSTIC_TRUNCATION_SUFFIX: &str = " [truncated]";

pub(super) fn bound_owned_diagnostic(mut value: String, maximum: usize) -> String {
    if value.len() <= maximum {
        return value;
    }
    let suffix = DIAGNOSTIC_TRUNCATION_SUFFIX;
    if maximum <= suffix.len() {
        value.clear();
        value.push_str(truncate_utf8(suffix, maximum));
        return value;
    }
    let prefix = truncate_utf8(&value, maximum - suffix.len()).len();
    value.truncate(prefix);
    value.push_str(suffix);
    value
}

pub(super) fn bound_formatted_diagnostic(
    arguments: std::fmt::Arguments<'_>,
    maximum: usize,
) -> String {
    struct Writer {
        value: String,
        maximum: usize,
        truncated: bool,
    }

    impl std::fmt::Write for Writer {
        fn write_str(&mut self, value: &str) -> std::fmt::Result {
            if self.truncated {
                return Ok(());
            }
            let remaining = self.maximum.saturating_sub(self.value.len());
            let retained = truncate_utf8(value, remaining);
            self.value.push_str(retained);
            self.truncated |= retained.len() != value.len();
            Ok(())
        }
    }

    let mut writer = Writer {
        value: String::new(),
        maximum,
        truncated: false,
    };
    std::fmt::write(&mut writer, arguments).expect("bounded diagnostic formatting cannot fail");
    if !writer.truncated {
        return writer.value;
    }
    let suffix = DIAGNOSTIC_TRUNCATION_SUFFIX;
    if maximum <= suffix.len() {
        return truncate_utf8(suffix, maximum).to_owned();
    }
    let prefix_bytes = truncate_utf8(&writer.value, maximum - suffix.len()).len();
    writer.value.truncate(prefix_bytes);
    writer.value.push_str(suffix);
    writer.value
}

pub(super) fn bound_error_diagnostic(error: MetaError, maximum: usize) -> MetaError {
    match error {
        MetaError::RuntimeTerminal(message) => {
            MetaError::RuntimeTerminal(bound_owned_diagnostic(message, maximum))
        }
        MetaError::InvalidConfig(message) => {
            MetaError::InvalidConfig(bound_owned_diagnostic(message, maximum))
        }
        MetaError::Activation(message) => {
            MetaError::Activation(bound_owned_diagnostic(message, maximum))
        }
        MetaError::Service(message) => MetaError::Service(bound_owned_diagnostic(message, maximum)),
        MetaError::Event(message) => MetaError::Event(bound_owned_diagnostic(message, maximum)),
        MetaError::InvalidInput(message) => {
            MetaError::InvalidInput(bound_owned_diagnostic(message, maximum))
        }
        error => error,
    }
}

pub(super) fn bound_event_callback_error(error: MetaError, maximum: usize) -> MetaError {
    match error {
        MetaError::Event(message) => MetaError::Event(bound_owned_diagnostic(message, maximum)),
        error => MetaError::Event(bound_formatted_diagnostic(format_args!("{error}"), maximum)),
    }
}

pub(super) fn bound_service_callback_error(error: MetaError, maximum: usize) -> MetaError {
    match error {
        MetaError::Service(message) => MetaError::Service(bound_owned_diagnostic(message, maximum)),
        error => MetaError::Service(bound_formatted_diagnostic(format_args!("{error}"), maximum)),
    }
}

#[cfg(test)]
mod lineage_tests {
    use super::*;
    use crate::PreparedActivation;

    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    struct ObservedCall {
        call: CallId,
        lineage: CallId,
        parent: Option<CallId>,
        origin: FiberId,
    }

    #[derive(Debug)]
    struct CaptureHandler(Arc<Mutex<Option<ObservedCall>>>);

    #[async_trait::async_trait]
    impl EventHandler for CaptureHandler {
        async fn handle(
            &self,
            invocation: InvocationContext,
            value: Arc<ConfigValue>,
        ) -> Result<EventOutcome> {
            *self.0.lock().expect("event lineage capture poisoned") = Some(ObservedCall {
                call: invocation.call_id(),
                lineage: invocation.lineage_call_id(),
                parent: invocation.parent_call_id(),
                origin: invocation.origin(),
            });
            Ok(EventOutcome::Continue(value.as_ref().clone()))
        }
    }

    #[derive(Debug)]
    struct ListenerFactory(Arc<Mutex<Option<ObservedCall>>>);

    #[async_trait::async_trait]
    impl PluginFactory for ListenerFactory {
        fn identity(&self) -> FactoryIdentity {
            FactoryIdentity::builtin("activation-lineage-listener", "1")
        }

        fn prepare(&self, desired: &ConfigValue) -> Result<PreparedActivation> {
            Ok(PreparedActivation::new(desired.clone()))
        }

        async fn activate(&self, plan: ActivationPlan) -> Result<()> {
            plan.context().on(
                "activation-lineage",
                Arc::new(CaptureHandler(Arc::clone(&self.0))),
                EventOptions::default(),
            )?;
            Ok(())
        }
    }

    #[derive(Debug)]
    struct DispatchFactory(Arc<Mutex<Option<CallId>>>);

    #[async_trait::async_trait]
    impl PluginFactory for DispatchFactory {
        fn identity(&self) -> FactoryIdentity {
            FactoryIdentity::builtin("activation-lineage-dispatcher", "1")
        }

        fn prepare(&self, desired: &ConfigValue) -> Result<PreparedActivation> {
            Ok(PreparedActivation::new(desired.clone()))
        }

        async fn activate(&self, plan: ActivationPlan) -> Result<()> {
            *self.0.lock().expect("activation lineage capture poisoned") =
                Some(plan.lineage_call_id());
            let receipt = plan
                .context()
                .dispatch("activation-lineage", DispatchMode::Emit, ConfigValue::Null)
                .await?;
            if receipt.invoked != 1 {
                return Err(MetaError::Event(
                    "activation lineage dispatch did not invoke one listener".to_owned(),
                ));
            }
            Ok(())
        }
    }

    #[tokio::test]
    async fn activation_context_event_uses_seed_with_no_parent() {
        let runtime = Runtime::default();
        let observed = Arc::new(Mutex::new(None));
        let listener = runtime
            .root()
            .apply(
                Arc::new(ListenerFactory(Arc::clone(&observed))),
                ConfigValue::Null,
            )
            .await
            .expect("listener activates");
        let activation_lineage = Arc::new(Mutex::new(None));
        let dispatcher = runtime
            .root()
            .apply(
                Arc::new(DispatchFactory(Arc::clone(&activation_lineage))),
                ConfigValue::Null,
            )
            .await
            .expect("dispatcher activates");

        let activation_lineage = activation_lineage
            .lock()
            .expect("activation lineage capture poisoned")
            .expect("activation exposes its lineage");
        let observed = observed
            .lock()
            .expect("event lineage capture poisoned")
            .expect("listener observes one event");
        assert_ne!(activation_lineage, CallId(0));
        assert_ne!(observed.call, activation_lineage);
        assert_eq!(observed.lineage, activation_lineage);
        assert_eq!(observed.parent, None);
        assert_eq!(observed.origin, dispatcher.id());

        assert!(dispatcher.dispose().await.is_clean());
        assert!(listener.dispose().await.is_clean());
        assert!(runtime.shutdown().await.is_complete());
    }
}
