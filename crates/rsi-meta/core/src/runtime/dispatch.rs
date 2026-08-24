#![allow(clippy::wildcard_imports)] // This is one implementation partition of runtime.

use super::*;

impl Runtime {
    #[allow(clippy::too_many_lines)] // Four modes share one snapshot and callback-admission seam.
    pub(super) async fn dispatch_event(
        &self,
        context: &Context,
        event: &EventKey,
        mode: DispatchMode,
        value: configuration::OwnedJsonValue,
        scope: Option<&ServiceKey>,
    ) -> Result<EventReceipt> {
        let _runtime_admission = self.begin_admission(false)?;
        let _dispatch = self.inner.resources.event_dispatches.try_reserve(1).ok_or(
            MetaError::CapacityExhausted {
                resource: "event dispatches",
            },
        )?;
        let deadline = tokio::time::Instant::now()
            .checked_add(self.inner.limits.deadlines.event_dispatch)
            .ok_or_else(|| {
                MetaError::InvalidInput("event dispatch deadline overflow".to_owned())
            })?;
        if let Some(owner) = context.owner {
            let fiber = self.owner_fiber(owner)?;
            let data = fiber.data.lock().expect("fiber state poisoned");
            Self::validate_owner_data(owner, &data, true)?;
            if !matches!(data.state, FiberState::Loading | FiberState::Active) {
                return Err(MetaError::StaleContext {
                    fiber: owner.fiber,
                    generation: owner.generation,
                });
            }
        }
        let (value, _) = configuration::validate_owned_json_payload(
            value,
            &self.inner.limits.payloads,
            self.inner.limits.payloads.maximum_frame_bytes,
        )?;
        let value = Arc::new(value.into_inner());
        let candidates = {
            let state = self.inner.state.lock().expect("runtime state poisoned");
            state
                .listeners
                .get(event)
                .map_or_else(Vec::new, ListenerRegistry::snapshot)
        };
        let listeners = candidates
            .into_iter()
            .filter(|listener| {
                let Some(scope) = scope else {
                    return true;
                };
                listener.options.global
                    || Self::isolation_for(&context.isolation, scope)
                        == Self::isolation_for(&listener.scope.isolation, scope)
            })
            .collect::<Vec<_>>();
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

    /// Removes one staged or published listener and every reverse ownership
    /// reference under one Runtime transaction.
    pub(super) fn remove_listener_owned(
        &self,
        owner: Owner,
        id: EventListenerId,
        cause: ListenerRemovalCause,
    ) -> bool {
        let mut state = self.inner.state.lock().expect("runtime state poisoned");
        let Some(event) = state.listener_events.get(&id).cloned() else {
            return false;
        };
        let published = state
            .listeners
            .get(&event)
            .and_then(|listeners| listeners.get(id))
            .cloned();
        let staged = state
            .staged_listeners
            .get(&(owner.fiber, owner.generation))
            .and_then(|listeners| listeners.get(&id))
            .cloned();
        let Some(listener) = published.as_ref().or(staged.as_ref()) else {
            return false;
        };
        if listener.owner != owner.fiber || listener.generation != owner.generation {
            return false;
        }
        let Some(fiber) = state.fibers.get(&owner.fiber).cloned() else {
            return false;
        };
        let mut data = fiber.data.lock().expect("fiber state poisoned");
        if data.generation != owner.generation
            || (cause == ListenerRemovalCause::Explicit
                && !matches!(data.state, FiberState::Loading | FiberState::Active))
        {
            return false;
        }
        let Some(active) = data.active.as_mut() else {
            return false;
        };
        if active.generation != owner.generation || !active.listeners.contains_key(&id) {
            return false;
        }

        if published.is_some() {
            let remove_bucket = {
                let listeners = state
                    .listeners
                    .get_mut(&event)
                    .expect("published listener event has a registry");
                let removed = listeners.remove(id);
                debug_assert!(removed.is_some());
                listeners.is_empty()
            };
            if remove_bucket {
                state.listeners.remove(&event);
            }
        } else {
            let key = (owner.fiber, owner.generation);
            let remove_staged = {
                let listeners = state
                    .staged_listeners
                    .get_mut(&key)
                    .expect("staged listener owner has a registry");
                let removed = listeners.remove(&id);
                debug_assert!(removed.is_some());
                listeners.is_empty()
            };
            if remove_staged {
                state.staged_listeners.remove(&key);
            }
        }
        active.listeners.remove(&id);
        state.listener_events.remove(&id);
        state.revision += 1;
        drop(data);
        drop(state);
        drop(published);
        drop(staged);
        true
    }

    /// Atomically consumes a once-listener immediately before invocation.
    /// Non-once listeners retain snapshot semantics.
    fn claim_listener(&self, listener: &ListenerBinding) -> bool {
        !listener.options.once
            || self.remove_listener_owned(
                Owner {
                    fiber: listener.owner,
                    generation: listener.generation,
                },
                listener.id,
                ListenerRemovalCause::Once,
            )
    }

    fn invoke_listener<'a>(
        &'a self,
        caller_context: &'a Context,
        caller: FiberId,
        listener: Arc<ListenerBinding>,
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
            if !self.claim_listener(&listener) {
                return Ok(None);
            }

            let (invocation, cancellation) =
                self.event_invocation(caller_context, caller, &listener);
            let terminal_cancellation = self.inner.terminal_cancellation.clone();
            let mut handler = Some(Box::pin(
                std::panic::AssertUnwindSafe(listener.handler.handle(invocation, value))
                    .catch_unwind(),
            ));
            let selected = tokio::select! {
                biased;
                () = terminal_cancellation.cancelled() => {
                    cancellation.cancel();
                    Err(self.terminal_error())
                }
                () = cancellation.cancelled() => Err(MetaError::Cancelled),
                () = &mut deadline_sleep => {
                    cancellation.cancel();
                    Err(MetaError::Timeout("event dispatch"))
                }
                result = handler
                    .as_mut()
                    .expect("the event-handler future lives through selection")
                    .as_mut() => match result {
                    Ok(result) => result.map_err(|error| self.bound_listener_error(error)),
                    Err(_) => Err(MetaError::Event("event handler panicked".to_owned())),
                },
            };
            let selected = selected.map(|outcome| match outcome {
                EventOutcome::Continue(value) => (false, configuration::OwnedJsonValue::new(value)),
                EventOutcome::Complete(value) => (true, configuration::OwnedJsonValue::new(value)),
            });
            let (complete, value) = if drop_catching_unwind(handler.take()) {
                return Err(MetaError::Event("event handler panicked".to_owned()));
            } else {
                selected?
            };
            Ok(Some(self.validate_event_outcome(complete, value)?))
        })
    }

    fn event_invocation(
        &self,
        caller_context: &Context,
        caller: FiberId,
        listener: &ListenerBinding,
    ) -> (InvocationContext, CancellationToken) {
        let call_id = CallId(self.inner.next_call.fetch_add(1, Ordering::AcqRel) + 1);
        let cancellation = CancellationToken::new();
        let origin = caller_context
            .trace
            .as_ref()
            .map_or(caller, |trace| trace.origin);
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
            isolation: Arc::clone(&listener.scope.isolation),
            intercepts: Arc::clone(&listener.scope.intercepts),
            entries: listener.scope.entries,
            encoded_bytes: listener.scope.encoded_bytes,
            trace: Some(CallTrace {
                origin,
                parent_call: Some(call_id),
            }),
        };
        (
            InvocationContext::new(
                call_id,
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
        )
    }

    fn validate_event_outcome(
        &self,
        complete: bool,
        value: configuration::OwnedJsonValue,
    ) -> Result<EventOutcome> {
        let (value, _) = configuration::validate_owned_json_payload(
            value,
            &self.inner.limits.payloads,
            self.inner.limits.payloads.maximum_frame_bytes,
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

    fn bound_listener_error(&self, error: MetaError) -> MetaError {
        bound_event_callback_error(error, self.inner.limits.payloads.maximum_diagnostic_bytes)
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
