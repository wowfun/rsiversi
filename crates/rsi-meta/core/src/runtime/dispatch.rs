#![allow(clippy::wildcard_imports)] // This is one implementation partition of runtime.

use super::*;

impl Runtime {
    #[allow(clippy::too_many_lines)] // Five modes share one immutable listener snapshot boundary.
    pub(super) async fn dispatch_event(
        &self,
        context: &Context,
        event: &EventKey,
        mode: DispatchMode,
        value: Value,
        scope: Option<&ServiceKey>,
    ) -> Result<EventReceipt> {
        self.ensure_admitting()?;
        let deadline = tokio::time::Instant::now() + self.inner.limits.event_callback_timeout;
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
        self.validate_event_value(&value)?;
        let value = Arc::new(value);
        let mut listeners = {
            let state = self.inner.state.lock().expect("runtime state poisoned");
            let candidates = state
                .listeners
                .get(event)
                .map_or_else(Vec::new, ListenerRegistry::snapshot);
            candidates
                .into_iter()
                .filter(|listener| {
                    let Some(scope) = scope else {
                        return true;
                    };
                    listener.options.global
                        || Self::isolation_for(&context.isolation, scope)
                            == Self::isolation_for(&listener.scope.isolation, scope)
                })
                .collect::<Vec<_>>()
        };
        let caller = context.owner.map_or(ROOT_FIBER, |owner| owner.fiber);
        match mode {
            DispatchMode::Parallel => {
                let admitted = listeners
                    .drain(..)
                    .filter_map(|listener| {
                        let lease = listener.lease.acquire(false)?;
                        Some((listener, lease))
                    })
                    .collect::<Vec<_>>();
                let claimed_once = self.claim_parallel_listeners(&admitted);
                let tasks = admitted
                    .into_iter()
                    .filter(|(listener, _)| {
                        !listener.options.once || claimed_once.contains(&listener.id)
                    })
                    .map(|(listener, lease)| {
                        self.invoke_listener(
                            context,
                            caller,
                            listener,
                            value.clone(),
                            lease,
                            deadline,
                        )
                    })
                    .collect::<Vec<_>>();
                let invoked = tasks.len();
                let results = join_all(tasks).await;
                let errors = results
                    .into_iter()
                    .filter_map(std::result::Result::err)
                    .map(|error| error.to_string())
                    .collect::<Vec<_>>();
                if !errors.is_empty() {
                    return Err(MetaError::Event(errors.join("; ")));
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
                    let Some(lease) = listener.lease.acquire(false) else {
                        continue;
                    };
                    if !self.claim_listener(&listener) {
                        continue;
                    }
                    invoked += 1;
                    match self
                        .invoke_listener(context, caller, listener, current, lease, deadline)
                        .await?
                    {
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
                    let Some(lease) = listener.lease.acquire(false) else {
                        continue;
                    };
                    if !self.claim_listener(&listener) {
                        continue;
                    }
                    invoked += 1;
                    match self
                        .invoke_listener(context, caller, listener, value.clone(), lease, deadline)
                        .await?
                    {
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
                    let Some(lease) = listener.lease.acquire(false) else {
                        continue;
                    };
                    if !self.claim_listener(&listener) {
                        continue;
                    }
                    invoked += 1;
                    self.invoke_listener(context, caller, listener, value.clone(), lease, deadline)
                        .await?;
                }
                Ok(EventReceipt {
                    invoked,
                    completed: None,
                })
            }
        }
    }

    /// Atomically consumes a once-listener immediately before invocation.
    /// Non-once listeners do not need a registry claim.
    fn claim_listener(&self, listener: &ListenerBinding) -> bool {
        if !listener.options.once {
            return true;
        }
        let mut state = self.inner.state.lock().expect("runtime state poisoned");
        if state.listener_events.get(&listener.id) != Some(&listener.event) {
            return false;
        }
        let Some(removed) = state
            .listeners
            .get_mut(&listener.event)
            .and_then(|listeners| listeners.remove(listener.id))
        else {
            return false;
        };
        debug_assert_eq!(removed.owner, listener.owner);
        debug_assert_eq!(removed.generation, listener.generation);
        state.listener_events.remove(&listener.id);
        state.revision += 1;
        true
    }

    /// Claims every admitted once-listener for one parallel dispatch under one
    /// registry lock. Serial modes must continue claiming immediately before
    /// each invocation so an early completion does not consume later listeners.
    fn claim_parallel_listeners(
        &self,
        admitted: &[(ListenerBinding, LeaseGuard)],
    ) -> BTreeSet<EventListenerId> {
        let requested = admitted
            .iter()
            .filter_map(|(listener, _)| listener.options.once.then_some(listener.id))
            .collect::<BTreeSet<_>>();
        if requested.is_empty() {
            return BTreeSet::new();
        }
        let event = &admitted
            .iter()
            .find(|(listener, _)| listener.options.once)
            .expect("a requested once-listener exists")
            .0
            .event;
        let mut state = self.inner.state.lock().expect("runtime state poisoned");
        let mut claimed = BTreeSet::new();
        for id in requested {
            if state.listener_events.get(&id) == Some(event)
                && state
                    .listeners
                    .get_mut(event)
                    .and_then(|listeners| listeners.remove(id))
                    .is_some()
            {
                claimed.insert(id);
            }
        }
        if claimed.is_empty() {
            return claimed;
        }
        for id in &claimed {
            state.listener_events.remove(id);
        }
        state.revision += 1;
        claimed
    }

    fn invoke_listener<'a>(
        &'a self,
        caller_context: &'a Context,
        caller: FiberId,
        listener: ListenerBinding,
        value: Arc<Value>,
        lease: LeaseGuard,
        deadline: tokio::time::Instant,
    ) -> BoxFuture<'a, Result<EventOutcome>> {
        Box::pin(async move {
            let _lease = lease;
            let call_id = CallId(self.inner.next_call.fetch_add(1, Ordering::AcqRel) + 1);
            let cancellation = CancellationToken::new();
            let provider_context = Context {
                runtime: self.clone(),
                owner: Some(Owner {
                    fiber: listener.owner,
                    generation: listener.generation,
                }),
                isolation: Arc::clone(&listener.scope.isolation),
                intercepts: Arc::clone(&listener.scope.intercepts),
                trace: Some(CallTrace {
                    origin: caller_context
                        .trace
                        .as_ref()
                        .map_or(caller, |trace| trace.origin),
                    parent_call: Some(call_id),
                }),
            };
            let invocation = InvocationContext::new(
                call_id,
                caller_context
                    .trace
                    .as_ref()
                    .and_then(|trace| trace.parent_call),
                caller_context
                    .trace
                    .as_ref()
                    .map_or(caller, |trace| trace.origin),
                caller,
                listener.owner,
                listener.generation,
                Arc::new(InterceptLayers::empty()),
                caller_context.clone(),
                provider_context,
                cancellation.clone(),
            );
            let terminal_cancellation = self.inner.terminal_cancellation.clone();
            let operation = async {
                tokio::select! {
                    biased;
                    () = terminal_cancellation.cancelled() => {
                        cancellation.cancel();
                        Err(self.ensure_admitting().expect_err(
                            "terminal cancellation is published after the terminal reason",
                        ))
                    }
                    () = cancellation.cancelled() => Err(MetaError::Cancelled),
                    result = std::panic::AssertUnwindSafe(
                        listener.handler.handle(invocation, value),
                    ).catch_unwind() => match result {
                        Ok(result) => result,
                        Err(_) => Err(MetaError::Event("event handler panicked".to_owned())),
                    },
                }
            };
            if let Ok(result) = tokio::time::timeout_at(deadline, operation).await {
                let outcome = result?;
                match &outcome {
                    EventOutcome::Continue(value) | EventOutcome::Complete(value) => {
                        self.validate_event_value(value)?;
                    }
                }
                Ok(outcome)
            } else {
                cancellation.cancel();
                Err(MetaError::Timeout("event dispatch"))
            }
        })
    }

    fn validate_event_value(&self, value: &Value) -> Result<()> {
        let encoded_bytes = configuration::encoded_json_size(value)
            .map_err(|error| MetaError::InvalidInput(error.to_string()))?;
        if encoded_bytes > self.inner.limits.maximum_frame_bytes {
            return Err(MetaError::PayloadTooLarge {
                maximum: self.inner.limits.maximum_frame_bytes,
            });
        }
        Ok(())
    }
}
