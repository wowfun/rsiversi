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

    #[must_use]
    /// Consumes this value and selects an explicit isolation for one service.
    ///
    /// Unbranched scope chains mutate their uniquely owned map in place; a
    /// cloned sibling causes copy-on-write and remains unchanged.
    pub fn isolate(mut self, service: impl Into<ServiceKey>, isolation: IsolationId) -> Self {
        Arc::make_mut(&mut self.isolation).insert(service.into(), isolation);
        self
    }

    /// Consumes this value and selects a newly allocated isolation identity.
    pub fn isolate_fresh(self, service: impl Into<ServiceKey>) -> (Self, IsolationId) {
        let isolation = IsolationId(
            self.runtime
                .inner
                .next_isolation
                .fetch_add(1, Ordering::AcqRel)
                + 1,
        );
        (self.isolate(service, isolation), isolation)
    }

    /// Consumes this value and appends one bounded direct-edge overlay layer.
    ///
    /// Only the selected layer list is copied when a cloned sibling shares it;
    /// the accumulated encoded length is maintained incrementally.
    pub fn intercept(mut self, service: impl Into<ServiceKey>, layer: Value) -> Result<Self> {
        let service = service.into();
        let layer_bytes = configuration::encoded_json_size(&layer)
            .map_err(|error| MetaError::InvalidInput(error.to_string()))?;
        let intercepts = Arc::make_mut(&mut self.intercepts);
        let layers = Arc::make_mut(
            intercepts
                .entry(service)
                .or_insert_with(|| Arc::new(InterceptLayers::empty())),
        );
        let separator_bytes = usize::from(!layers.values.is_empty());
        let encoded_bytes = layers
            .encoded_bytes
            .checked_add(separator_bytes)
            .and_then(|size| size.checked_add(layer_bytes))
            .ok_or(MetaError::PayloadTooLarge {
                maximum: self.runtime.inner.limits.maximum_frame_bytes,
            })?;
        if encoded_bytes > self.runtime.inner.limits.maximum_frame_bytes {
            return Err(MetaError::PayloadTooLarge {
                maximum: self.runtime.inner.limits.maximum_frame_bytes,
            });
        }
        layers.values.push(layer);
        layers.encoded_bytes = encoded_bytes;
        Ok(self)
    }

    /// Prepares and applies a factory as a child of this Context.
    ///
    /// This future must be polled inside a Tokio runtime. Once Fiber insertion
    /// begins, Runtime-owned rollback completes even if the caller drops it.
    pub async fn apply(
        &self,
        factory: Arc<dyn PluginFactory>,
        config: ConfigValue,
    ) -> Result<FiberHandle> {
        let runtime = self.runtime.clone();
        let prepared = tokio::task::spawn_blocking(move || runtime.prepare(factory, config))
            .await
            .map_err(|error| {
                MetaError::Activation(format!("plugin preparation task failed: {error}"))
            })??;
        self.runtime.apply_prepared(self, prepared).await
    }

    /// Applies an already validated and normalized preparation proof.
    ///
    /// This future must be polled inside a Tokio runtime.
    pub async fn apply_prepared(&self, prepared: PreparedPlugin) -> Result<FiberHandle> {
        self.runtime.apply_prepared(self, prepared).await
    }

    /// Registers one reverse-ordered cleanup effect on the owning generation.
    pub fn defer(&self, label: impl Into<String>, cleanup: Cleanup) -> Result<()> {
        let owner = self.owner.ok_or_else(|| {
            MetaError::InvalidInput("the root context cannot own an effect".to_owned())
        })?;
        self.runtime.add_effect(owner, label.into(), cleanup)
    }

    /// Stages an endpoint for one service declared by the owning factory.
    pub fn provide(
        &self,
        key: impl Into<ServiceKey>,
        contract: impl Into<ContractId>,
        version: ContractVersion,
        endpoint: Arc<dyn ServiceEndpoint>,
    ) -> Result<()> {
        self.runtime
            .provide(self, key.into(), contract.into(), version, endpoint)
    }

    /// Captures the exact service binding resolved for the owning generation.
    pub fn service(&self, key: impl Into<ServiceKey>) -> Result<ServiceHandle> {
        self.runtime.service(self, &key.into())
    }

    /// Stages one event listener owned by this Context generation.
    pub fn on(
        &self,
        event: impl Into<EventKey>,
        handler: Arc<dyn EventHandler>,
        options: EventOptions,
    ) -> Result<EventListenerId> {
        self.runtime
            .add_listener(self, event.into(), handler, options)
    }

    /// Removes a listener when this Context still has authority over it.
    pub fn off(&self, listener: EventListenerId) -> bool {
        self.runtime.remove_listener(self, listener)
    }

    /// Dispatches a bounded event without service-isolation scoping.
    ///
    /// This future must be polled inside a Tokio runtime.
    pub async fn dispatch(
        &self,
        event: impl Into<EventKey>,
        mode: DispatchMode,
        value: Value,
    ) -> Result<EventReceipt> {
        self.runtime
            .dispatch_event(self, &event.into(), mode, value, None)
            .await
    }

    /// Dispatches a bounded event in the selected service-isolation scope.
    ///
    /// This future must be polled inside a Tokio runtime.
    pub async fn dispatch_scoped(
        &self,
        scope: impl Into<ServiceKey>,
        event: impl Into<EventKey>,
        mode: DispatchMode,
        value: Value,
    ) -> Result<EventReceipt> {
        let scope = scope.into();
        self.runtime
            .dispatch_event(self, &event.into(), mode, value, Some(&scope))
            .await
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
    /// This future must be polled inside a Tokio runtime. Once admitted, the
    /// transaction remains Runtime-owned if the initiating future is dropped.
    pub async fn reconfigure(&self, config: ConfigValue) -> Result<FiberSnapshot> {
        let runtime = self.runtime.clone();
        let fiber = Arc::clone(&self.fiber);
        tokio::spawn(async move {
            let _configuration = fiber.configuration.lock().await;
            runtime.ensure_admitting()?;
            let factory = Arc::clone(&fiber.data.lock().expect("fiber state poisoned").factory);
            let maximum_config_bytes = runtime.inner.limits.maximum_config_bytes;
            let config = tokio::task::spawn_blocking(move || {
                Runtime::normalize_config(&factory, config, maximum_config_bytes)
            })
            .await
            .map_err(|error| {
                MetaError::InvalidConfig(format!("validation task failed: {error}"))
            })??;
            {
                let mut data = fiber.data.lock().expect("fiber state poisoned");
                if data.disposed {
                    return Err(MetaError::FiberDisposed { fiber: fiber.id });
                }
                data.config = config;
                data.target_revision += 1;
                if matches!(data.state, FiberState::Failed(_)) {
                    data.state = FiberState::Pending(Vec::new());
                    data.last_attempt = None;
                }
            }
            runtime.reconcile_fiber(fiber.id).await;
            Ok(fiber.snapshot())
        })
        .await
        .map_err(|error| MetaError::Activation(format!("reconfiguration task failed: {error}")))?
    }

    /// Joins idempotent child/effect teardown and returns every cleanup failure.
    ///
    /// This future must be polled inside a Tokio runtime. Once initiated,
    /// disposal remains Runtime-owned if this future is dropped.
    pub async fn dispose(&self) -> CleanupReport {
        if let Some(report) = self
            .fiber
            .data
            .lock()
            .expect("fiber state poisoned")
            .disposal_report
            .clone()
        {
            return report;
        }
        let runtime = self.runtime.clone();
        let disposal_runtime = runtime.clone();
        let id = self.id();
        let fiber = Arc::clone(&self.fiber);
        match tokio::spawn(async move { disposal_runtime.dispose_fiber_instance(fiber).await })
            .await
        {
            Ok(report) => report,
            Err(error) => {
                runtime.mark_terminal(format!("runtime-owned Fiber disposal task failed: {error}"));
                let mut report = CleanupReport::default();
                report.push(
                    format!("fiber {} disposal", id.0),
                    MetaError::Activation(format!("disposal task failed: {error}")),
                );
                report
            }
        }
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
            factory: self.descriptor.identity.clone(),
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
