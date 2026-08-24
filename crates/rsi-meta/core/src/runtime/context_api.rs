#![allow(clippy::wildcard_imports)] // This is one implementation partition of runtime.

use super::*;

impl Context {
    /// Returns the retained Runtime that owns this scope.
    pub fn runtime(&self) -> &Runtime {
        &self.runtime
    }

    /// Returns the owning Fiber generation, or `None` for a root Context.
    pub fn owner(&self) -> Option<(FiberId, FiberGeneration)> {
        self.owner.map(|owner| (owner.fiber, owner.generation))
    }

    /// Consumes this value and selects an explicit isolation for one service.
    ///
    /// Unbranched scope chains mutate their uniquely owned map in place; a
    /// cloned sibling causes copy-on-write and remains unchanged.
    pub fn isolate(self, service: impl AsRef<str>, isolation: IsolationId) -> Result<Self> {
        let _runtime_admission = self.runtime.begin_admission(false)?;
        self.isolate_admitted(service.as_ref(), isolation)
    }

    fn isolate_admitted(mut self, service: &str, isolation: IsolationId) -> Result<Self> {
        self.validate_context_key(service)?;
        let service = ServiceKey::new(service);
        if !self.isolation.contains_key(&service) {
            self.entries = self.next_context_entry_count()?;
            self.encoded_bytes = self.next_context_bytes(
                service
                    .as_str()
                    .len()
                    .checked_add(std::mem::size_of::<IsolationId>())
                    .ok_or(MetaError::PayloadTooLarge {
                        maximum: self.runtime.inner.limits.payloads.maximum_context_bytes,
                    })?,
            )?;
        }
        Arc::make_mut(&mut self.isolation).insert(service, isolation);
        Ok(self)
    }

    /// Consumes this value and selects a newly allocated isolation identity.
    pub fn isolate_fresh(self, service: impl AsRef<str>) -> Result<(Self, IsolationId)> {
        let _runtime_admission = self.runtime.begin_admission(false)?;
        let isolation = IsolationId(
            self.runtime
                .inner
                .next_isolation
                .fetch_add(1, Ordering::AcqRel)
                + 1,
        );
        Ok((
            self.isolate_admitted(service.as_ref(), isolation)?,
            isolation,
        ))
    }

    /// Consumes this value and appends one bounded direct-edge overlay layer.
    ///
    /// Only the selected layer list is copied when a cloned sibling shares it;
    /// the accumulated encoded length is maintained incrementally.
    pub fn intercept(mut self, service: impl AsRef<str>, layer: Value) -> Result<Self> {
        let layer = configuration::OwnedJsonValue::new(layer);
        let _runtime_admission = self.runtime.begin_admission(false)?;
        let service = service.as_ref();
        self.validate_context_key(service)?;
        let service = ServiceKey::new(service);
        let payloads = &self.runtime.inner.limits.payloads;
        let (layer, layer_bytes) = configuration::validate_owned_json_payload(
            layer,
            payloads,
            payloads.maximum_frame_bytes,
        )?;
        let existing = self.intercepts.get(&service);
        let is_new_entry = existing.is_none();
        let previous_service_bytes = existing.map_or(2, |layers| layers.encoded_bytes);
        let separator_bytes = usize::from(existing.is_some_and(|layers| !layers.values.is_empty()));
        let encoded_bytes = previous_service_bytes
            .checked_add(separator_bytes)
            .and_then(|size| size.checked_add(layer_bytes))
            .ok_or(MetaError::PayloadTooLarge {
                maximum: payloads.maximum_frame_bytes,
            })?;
        if encoded_bytes > payloads.maximum_frame_bytes {
            return Err(MetaError::PayloadTooLarge {
                maximum: payloads.maximum_frame_bytes,
            });
        }
        let key_bytes = if is_new_entry {
            configuration::encoded_json_size_bounded(
                service.as_str(),
                payloads.maximum_context_bytes,
            )
            .map_err(|_| MetaError::PayloadTooLarge {
                maximum: payloads.maximum_context_bytes,
            })?
        } else {
            0
        };
        let added_bytes = key_bytes
            .checked_add(if is_new_entry { 2 } else { 0 })
            .and_then(|size| size.checked_add(separator_bytes))
            .and_then(|size| size.checked_add(layer_bytes))
            .ok_or(MetaError::PayloadTooLarge {
                maximum: payloads.maximum_context_bytes,
            })?;
        let context_bytes = self.next_context_bytes(added_bytes)?;
        if is_new_entry {
            self.entries = self.next_context_entry_count()?;
        }
        let intercepts = Arc::make_mut(&mut self.intercepts);
        let layers = Arc::make_mut(
            intercepts
                .entry(service)
                .or_insert_with(|| Arc::new(InterceptLayers::empty())),
        );
        layers.values.push(layer.into_inner());
        layers.encoded_bytes = encoded_bytes;
        self.encoded_bytes = context_bytes;
        Ok(self)
    }

    fn validate_context_key(&self, service: &str) -> Result<()> {
        if service.len() > self.runtime.inner.limits.payloads.maximum_identifier_bytes {
            return Err(MetaError::InvalidInput(
                "context service identifier exceeds the configured byte limit".to_owned(),
            ));
        }
        Ok(())
    }

    fn validate_event_key(&self, event: &str) -> Result<()> {
        if event.len() > self.runtime.inner.limits.payloads.maximum_identifier_bytes {
            return Err(MetaError::InvalidInput(
                "event identifier exceeds the configured byte limit".to_owned(),
            ));
        }
        Ok(())
    }

    fn next_context_entry_count(&self) -> Result<usize> {
        let entries = self
            .entries
            .checked_add(1)
            .ok_or(MetaError::CapacityExhausted {
                resource: "context entries",
            })?;
        if entries > self.runtime.inner.limits.topology.maximum_context_entries {
            return Err(MetaError::CapacityExhausted {
                resource: "context entries",
            });
        }
        Ok(entries)
    }

    fn next_context_bytes(&self, added: usize) -> Result<usize> {
        let maximum = self.runtime.inner.limits.payloads.maximum_context_bytes;
        let bytes = self
            .encoded_bytes
            .checked_add(added)
            .ok_or(MetaError::PayloadTooLarge { maximum })?;
        if bytes > maximum {
            return Err(MetaError::PayloadTooLarge { maximum });
        }
        Ok(bytes)
    }

    /// Prepares and applies a factory as a child of this Context.
    ///
    /// This future must be polled inside a Tokio runtime. One absolute
    /// transition deadline includes preparation and convergence. Timeout or
    /// caller cancellation cannot stop blocking work or Runtime-owned rollback.
    pub fn apply(
        &self,
        factory: Arc<dyn PluginFactory>,
        config: ConfigValue,
    ) -> impl std::future::Future<Output = Result<FiberHandle>> + '_ {
        let config = configuration::OwnedJsonValue::new(config);
        async move {
            let deadline = tokio::time::Instant::now()
                .checked_add(self.runtime.inner.limits.deadlines.transition)
                .expect("validated transition deadline fits Tokio Instant");
            let maximum_diagnostic_bytes =
                self.runtime.inner.limits.payloads.maximum_diagnostic_bytes;
            let runtime = self.runtime.clone();
            let preparation = runtime.begin_plugin_preparation()?;
            let preparation = self.runtime.yield_reconciliation_slot(async move {
                tokio::task::spawn_blocking(move || {
                    runtime.prepare_admitted(factory, config, preparation)
                })
                .await
                .map_err(|error| {
                    MetaError::Activation(super::dispatch::bound_formatted_diagnostic(
                        format_args!("plugin preparation task failed: {error}"),
                        maximum_diagnostic_bytes,
                    ))
                })?
                .map_err(|error| {
                    super::dispatch::bound_error_diagnostic(error, maximum_diagnostic_bytes)
                })
            });
            let prepared = tokio::time::timeout_at(deadline, preparation)
                .await
                .map_err(|_| MetaError::Timeout("plugin transition"))??;
            tokio::time::timeout_at(deadline, self.runtime.apply_prepared(self, prepared))
                .await
                .map_err(|_| MetaError::Timeout("plugin transition"))?
        }
    }

    /// Applies an already validated and normalized preparation proof.
    ///
    /// This future must be polled inside a Tokio runtime. Its absolute
    /// transition deadline drops only the waiter; an inserted unacknowledged
    /// Fiber remains Runtime-owned through disposal.
    pub async fn apply_prepared(&self, prepared: PreparedPlugin) -> Result<FiberHandle> {
        let deadline = tokio::time::Instant::now()
            .checked_add(self.runtime.inner.limits.deadlines.transition)
            .expect("validated transition deadline fits Tokio Instant");
        tokio::time::timeout_at(deadline, self.runtime.apply_prepared(self, prepared))
            .await
            .map_err(|_| MetaError::Timeout("plugin transition"))?
    }

    /// Registers one reverse-ordered cleanup effect on the owning generation.
    pub fn defer(&self, label: impl Into<String>, cleanup: Cleanup) -> Result<()> {
        let owner = self.owner.ok_or_else(|| {
            MetaError::InvalidInput("the root context cannot own an effect".to_owned())
        })?;
        let label = label.into();
        if label.len() > self.runtime.inner.limits.payloads.maximum_diagnostic_bytes {
            return Err(MetaError::InvalidInput(
                "effect label exceeds the configured diagnostic byte limit".to_owned(),
            ));
        }
        self.runtime.add_effect(owner, label, cleanup)
    }

    /// Stages an endpoint for one service declared by the owning factory.
    pub fn provide(
        &self,
        key: impl AsRef<str>,
        contract: impl AsRef<str>,
        version: ContractVersion,
        endpoint: Arc<dyn ServiceEndpoint>,
    ) -> Result<()> {
        let key = key.as_ref();
        if key.len() > self.runtime.inner.limits.payloads.maximum_identifier_bytes {
            return Err(MetaError::InvalidInput(
                "service identifier exceeds the configured byte limit".to_owned(),
            ));
        }
        let contract = contract.as_ref();
        if contract.len() > self.runtime.inner.limits.payloads.maximum_identifier_bytes {
            return Err(MetaError::InvalidInput(
                "service contract identifier exceeds the configured byte limit".to_owned(),
            ));
        }
        self.runtime.provide(
            self,
            ServiceKey::new(key),
            ContractId::new(contract),
            version,
            endpoint,
        )
    }

    /// Captures the exact service binding resolved for the owning generation.
    pub fn service(&self, key: impl AsRef<str>) -> Result<ServiceHandle> {
        let key = key.as_ref();
        if key.len() > self.runtime.inner.limits.payloads.maximum_identifier_bytes {
            return Err(MetaError::InvalidInput(
                "service identifier exceeds the configured byte limit".to_owned(),
            ));
        }
        self.runtime.service(self, &ServiceKey::new(key))
    }

    /// Stages one event listener owned by this Context generation.
    pub fn on(
        &self,
        event: impl AsRef<str>,
        handler: Arc<dyn EventHandler>,
        options: EventOptions,
    ) -> Result<EventListenerId> {
        let event = event.as_ref();
        self.validate_event_key(event)?;
        let event = EventKey::new(event);
        self.runtime.add_listener(self, event, handler, options)
    }

    /// Removes a listener when this Context still has authority over it.
    pub fn off(&self, listener: EventListenerId) -> bool {
        self.runtime.remove_listener(self, listener)
    }

    /// Dispatches a bounded event without service-isolation scoping.
    ///
    /// This future must be polled inside a Tokio runtime.
    pub fn dispatch<'a>(
        &'a self,
        event: impl AsRef<str> + 'a,
        mode: DispatchMode,
        value: Value,
    ) -> impl std::future::Future<Output = Result<EventReceipt>> + 'a {
        let value = configuration::OwnedJsonValue::new(value);
        async move {
            let event = event.as_ref();
            self.validate_event_key(event)?;
            let event = EventKey::new(event);
            self.runtime
                .dispatch_event(self, &event, mode, value, None)
                .await
        }
    }

    /// Dispatches a bounded event in the selected service-isolation scope.
    ///
    /// This future must be polled inside a Tokio runtime.
    pub fn dispatch_scoped<'a>(
        &'a self,
        scope: impl AsRef<str> + 'a,
        event: impl AsRef<str> + 'a,
        mode: DispatchMode,
        value: Value,
    ) -> impl std::future::Future<Output = Result<EventReceipt>> + 'a {
        let value = configuration::OwnedJsonValue::new(value);
        async move {
            let scope = scope.as_ref();
            self.validate_context_key(scope)?;
            let scope = ServiceKey::new(scope);
            let event = event.as_ref();
            self.validate_event_key(event)?;
            let event = EventKey::new(event);
            self.runtime
                .dispatch_event(self, &event, mode, value, Some(&scope))
                .await
        }
    }
}

impl FiberHandle {
    /// Returns the stable Runtime-local Fiber identity.
    pub fn id(&self) -> FiberId {
        self.fiber.id
    }

    /// Returns the Fiber's current observable state.
    pub fn snapshot(&self) -> FiberSnapshot {
        self.fiber.snapshot()
    }

    /// Subscribes to later Fiber snapshots.
    pub fn subscribe(&self) -> watch::Receiver<FiberSnapshot> {
        self.fiber.watch.subscribe()
    }

    /// Waits until the Fiber is not loading or unloading.
    ///
    /// This future must be polled inside a Tokio runtime.
    pub async fn wait_settled(&self) -> FiberSnapshot {
        let mut receiver = self.subscribe();
        loop {
            let snapshot = receiver.borrow().clone();
            if !snapshot.state.is_transitioning() {
                return snapshot;
            }
            if receiver.changed().await.is_err() {
                return self.snapshot();
            }
        }
    }

    /// Waits for an active generation, terminal Fiber state, or cancellation.
    ///
    /// This future must be polled inside a Tokio runtime.
    pub async fn wait_active(&self, cancellation: &CancellationToken) -> Result<FiberSnapshot> {
        let mut receiver = self.subscribe();
        loop {
            let snapshot = receiver.borrow().clone();
            match &snapshot.state {
                FiberState::Active => return Ok(snapshot),
                FiberState::Failed(error) => {
                    return Err(MetaError::Activation(error.clone()));
                }
                FiberState::Disposed => {
                    return Err(MetaError::FiberDisposed { fiber: self.id() });
                }
                FiberState::Pending(_) | FiberState::Loading | FiberState::Unloading => {}
            }
            tokio::select! {
                () = cancellation.cancelled() => return Err(MetaError::Cancelled),
                changed = receiver.changed() => {
                    if changed.is_err() {
                        return Err(MetaError::FiberDisposed { fiber: self.id() });
                    }
                }
            }
        }
    }

    /// Replaces retained configuration and converges one serialized generation.
    ///
    /// This future must be polled inside a Tokio runtime. One absolute
    /// transition deadline includes normalization and convergence. Once
    /// admitted, timeout or caller cancellation detaches only the waiter.
    pub fn reconfigure(
        &self,
        config: ConfigValue,
    ) -> impl std::future::Future<Output = Result<FiberSnapshot>> + '_ {
        let config = configuration::OwnedJsonValue::new(config);
        async move {
            let deadline = tokio::time::Instant::now()
                .checked_add(self.runtime.inner.limits.deadlines.transition)
                .expect("validated transition deadline fits Tokio Instant");
            let maximum_diagnostic_bytes =
                self.runtime.inner.limits.payloads.maximum_diagnostic_bytes;
            let runtime = self.runtime.clone();
            let fiber = Arc::clone(&self.fiber);
            let configuration =
                Arc::clone(&fiber.configuration)
                    .try_lock_owned()
                    .map_err(|_| MetaError::Busy {
                        operation: "plugin reconfiguration",
                    })?;
            let preparation = runtime.begin_preparation()?;
            let staged_config = runtime
                .inner
                .resources
                .retained_plugin_bytes
                .try_reserve(runtime.inner.limits.payloads.maximum_config_bytes)
                .ok_or(MetaError::CapacityExhausted {
                    resource: "retained plugin bytes",
                })?;
            let operation = tokio::spawn(async move {
                let (runtime_admission, preparation) = preparation.into_parts();
                let _runtime_admission = runtime_admission;
                let _configuration = configuration;
                let factory = {
                    let data = fiber.data.lock().expect("fiber state poisoned");
                    if data.disposed {
                        return Err(MetaError::FiberDisposed { fiber: fiber.id });
                    }
                    Arc::clone(
                        data.factory
                            .as_ref()
                            .expect("registered Fiber retains its factory"),
                    )
                };
                let payloads = runtime.inner.limits.payloads.clone();
                let (config, staged_config) = tokio::task::spawn_blocking(move || {
                    let _preparation = preparation;
                    let config = Runtime::normalize_config(&factory, config, &payloads)?;
                    let mut staged_config = staged_config;
                    staged_config.shrink_to(config.encoded_bytes);
                    Ok::<_, MetaError>((config, staged_config))
                })
                .await
                .map_err(|error| {
                    MetaError::InvalidConfig(super::dispatch::bound_formatted_diagnostic(
                        format_args!("validation task failed: {error}"),
                        maximum_diagnostic_bytes,
                    ))
                })?
                .map_err(|error| {
                    super::dispatch::bound_error_diagnostic(error, maximum_diagnostic_bytes)
                })?;
                {
                    let mut data = fiber.data.lock().expect("fiber state poisoned");
                    if data.disposed {
                        return Err(MetaError::FiberDisposed { fiber: fiber.id });
                    }
                    let previous = data
                        .config
                        .replace(Arc::new(RetainedConfig::new(config.value, staged_config)));
                    drop(previous);
                    data.target_revision += 1;
                    if matches!(data.state, FiberState::Failed(_)) {
                        data.state = FiberState::Pending(PendingReport::default());
                        data.last_attempt = None;
                    }
                }
                let snapshot = if let Some(ticket) = runtime.request_reconciliation(fiber.id) {
                    ticket.join().await
                } else {
                    fiber.snapshot()
                };
                Ok(snapshot)
            });
            tokio::time::timeout_at(deadline, operation)
                .await
                .map_err(|_| MetaError::Timeout("plugin transition"))?
                .map_err(|error| {
                    MetaError::Activation(super::dispatch::bound_formatted_diagnostic(
                        format_args!("reconfiguration task failed: {error}"),
                        maximum_diagnostic_bytes,
                    ))
                })?
                .map_err(|error| {
                    super::dispatch::bound_error_diagnostic(error, maximum_diagnostic_bytes)
                })
        }
    }

    /// Joins idempotent child/effect teardown and returns every cleanup failure.
    ///
    /// This future must be polled inside a Tokio runtime. Once initiated,
    /// disposal remains Runtime-owned if this future is dropped.
    pub async fn dispose(&self) -> CleanupReport {
        self.runtime
            .dispose_fiber_instance(Arc::clone(&self.fiber))
            .await
    }
}

impl fmt::Debug for FiberHandle {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("FiberHandle")
            .field("snapshot", &self.snapshot())
            .finish()
    }
}

impl Fiber {
    pub(super) fn context(&self, generation: FiberGeneration) -> Context {
        Context {
            runtime: Runtime {
                inner: self.runtime.upgrade().expect("runtime outlives fibers"),
            },
            owner: Some(Owner {
                fiber: self.id,
                generation,
            }),
            isolation: Arc::clone(&self.base_context.isolation),
            intercepts: Arc::clone(&self.base_context.intercepts),
            entries: self.base_context.entries,
            encoded_bytes: self.base_context.encoded_bytes,
            trace: self.base_context.trace.clone(),
        }
    }

    pub(super) fn snapshot(&self) -> FiberSnapshot {
        self.data
            .lock()
            .expect("fiber state poisoned")
            .snapshot(self.id)
    }

    pub(super) fn set_state(&self, state: FiberState) {
        let mut data = self.data.lock().expect("fiber state poisoned");
        data.state = state;
        let snapshot = data.snapshot(self.id);
        self.watch.send_replace(snapshot);
    }
}

impl FiberData {
    pub(super) fn snapshot(&self, id: FiberId) -> FiberSnapshot {
        FiberSnapshot {
            id,
            generation: self.generation,
            factory: self.identity.clone(),
            state: self.state.clone(),
        }
    }
}

pub(super) fn binding_identities(
    bindings: &BTreeMap<ServiceKey, Arc<ProviderBinding>>,
) -> BTreeMap<ServiceKey, (FiberId, FiberGeneration)> {
    bindings
        .iter()
        .map(|(key, binding)| (key.clone(), (binding.provider, binding.generation)))
        .collect()
}
