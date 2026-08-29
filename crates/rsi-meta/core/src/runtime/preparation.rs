#![allow(clippy::wildcard_imports)] // This is one implementation partition of runtime.

use super::*;
use crate::Requirement;
use crate::plugin::{LocalRequirement, PreparedState};
use crate::service::LeaseGuard;
use limits::{ResourceReservation, RuntimeResources};
use std::ops::Deref;

/// Opaque Runtime-bound proof that identity capture, resource reservation, and
/// one attempt preparation completed successfully exactly once.
///
/// The proof retains Runtime admission and its pessimistic reservations until
/// it is consumed by `apply_prepared` or dropped. Shutdown remains incomplete
/// while an unapplied proof is still live.
pub struct PreparedPlugin {
    pub(super) runtime: Weak<RuntimeInner>,
    pub(super) admission: LeaseGuard,
    pub(super) identity: FactoryIdentity,
    pub(super) update_mode: UpdateMode,
    pub(super) factory: RetainedFactory,
    pub(super) desired: DesiredConfig,
    pub(super) attempt: PreparedAttempt,
    pub(super) fiber_reservation: FiberReservation,
}

impl fmt::Debug for PreparedPlugin {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PreparedPlugin")
            .field("identity", &self.identity)
            .finish_non_exhaustive()
    }
}

pub(super) struct RetainedFactory {
    value: Option<Arc<dyn PluginFactory>>,
}

impl RetainedFactory {
    pub(super) fn new(value: Arc<dyn PluginFactory>) -> Self {
        Self { value: Some(value) }
    }
}

impl Clone for RetainedFactory {
    fn clone(&self) -> Self {
        Self {
            value: Some(Arc::clone(
                self.value
                    .as_ref()
                    .expect("retained factory remains available until destruction"),
            )),
        }
    }
}

impl Deref for RetainedFactory {
    type Target = dyn PluginFactory;

    fn deref(&self) -> &Self::Target {
        self.value
            .as_deref()
            .expect("retained factory remains available until destruction")
    }
}

impl Drop for RetainedFactory {
    fn drop(&mut self) {
        if let Some(value) = self.value.take() {
            drop_catching_unwind(value);
        }
    }
}

pub(super) struct DesiredConfig {
    pub(super) revision: u64,
    pub(super) value: Arc<RetainedConfig>,
}

pub(super) struct PreparedAttempt {
    pub(super) id: u64,
    pub(super) desired_revision: u64,
    pub(super) requirements: Arc<[Requirement]>,
    pub(super) local_requirements: Arc<[LocalRequirement]>,
    pub(super) config: Arc<RetainedConfig>,
    pub(super) state: Option<PreparedState>,
    pub(super) consumed: bool,
    // Retains the declared state charge even after `state` moves into
    // activation: the plugin may move that value into generation-owned work.
    _reservations: AttemptReservations,
}

impl PreparedAttempt {
    pub(super) fn required_services(&self) -> impl Iterator<Item = &ServiceKey> {
        self.requirements.iter().map(|requirement| &requirement.key)
    }

    pub(super) fn required_local_services(&self) -> impl Iterator<Item = &LocalRequirement> {
        self.local_requirements.iter()
    }
}

pub(super) struct FiberReservation {
    _fiber: ResourceReservation,
    identity_bytes: Option<ResourceReservation>,
}

pub(super) struct AttemptReservations {
    retained_plugin_bytes: ResourceReservation,
    dependency_edges: ResourceReservation,
}

pub(super) struct PreparationAdmission {
    runtime: LeaseGuard,
    preparation: PreparationPermit,
}

pub(super) struct PreparationPermit {
    _usage: ResourceReservation,
    _admission: tokio::sync::OwnedSemaphorePermit,
}

pub(super) struct PluginPreparation {
    admission: PreparationAdmission,
    fiber_reservation: FiberReservation,
    attempt_reservations: AttemptReservations,
}

impl PreparationAdmission {
    pub(super) fn into_parts(self) -> (LeaseGuard, PreparationPermit) {
        (self.runtime, self.preparation)
    }
}

impl FiberReservation {
    fn reserve(resources: &RuntimeResources) -> Result<Self> {
        let fiber = resources
            .fibers
            .try_reserve(1)
            .ok_or(MetaError::CapacityExhausted { resource: "fibers" })?;
        Ok(Self {
            _fiber: fiber,
            identity_bytes: None,
        })
    }

    fn reserve_identity(&mut self, resources: &RuntimeResources, bytes: usize) -> Result<()> {
        debug_assert!(self.identity_bytes.is_none());
        self.identity_bytes = Some(resources.retained_plugin_bytes.try_reserve(bytes).ok_or(
            MetaError::CapacityExhausted {
                resource: "retained plugin bytes",
            },
        )?);
        Ok(())
    }
}

impl AttemptReservations {
    fn reserve(
        resources: &RuntimeResources,
        retained_bytes: usize,
        dependency_edges: usize,
    ) -> Result<Self> {
        let retained_plugin_bytes = resources
            .retained_plugin_bytes
            .try_reserve(retained_bytes)
            .ok_or(MetaError::CapacityExhausted {
                resource: "retained plugin bytes",
            })?;
        let dependency_edges = resources
            .dependency_edges
            .try_reserve(dependency_edges)
            .ok_or(MetaError::CapacityExhausted {
                resource: "dependency edges",
            })?;
        Ok(Self {
            retained_plugin_bytes,
            dependency_edges,
        })
    }

    fn shrink_to(&mut self, retained_bytes: usize, dependency_edges: usize) {
        self.retained_plugin_bytes.shrink_to(retained_bytes);
        self.dependency_edges.shrink_to(dependency_edges);
    }

    fn split_retained_config(&mut self, non_config_bytes: usize) -> ResourceReservation {
        self.retained_plugin_bytes.split_off(non_config_bytes)
    }
}

impl Runtime {
    /// Retains bounded resolved identity, reserves retained resources, and
    /// prepares one attempt from the unchanged desired configuration.
    ///
    /// The returned proof belongs to this Runtime and can be consumed by
    /// [`Context::apply_prepared`] without invoking preparation again.
    pub fn prepare(&self, factory: ResolvedFactory, config: ConfigValue) -> Result<PreparedPlugin> {
        let preparation = self.begin_plugin_preparation()?;
        let (identity, update_mode, implementation) = factory.into_parts();
        let factory = RetainedFactory::new(implementation);
        let config = configuration::OwnedJsonValue::new(config);
        self.prepare_admitted(identity, update_mode, factory, config, preparation)
    }

    pub(super) fn begin_preparation(&self) -> Result<PreparationAdmission> {
        let runtime = self.begin_admission(false)?;
        let admission = Arc::clone(&self.inner.preparation_admission)
            .try_acquire_owned()
            .map_err(|_| {
                self.inner.resources.preparations.record_rejection();
                MetaError::Busy {
                    operation: "plugin preparation",
                }
            })?;
        let usage = self
            .inner
            .resources
            .preparations
            .try_reserve(1)
            .expect("preparation semaphore and resource ledger stay synchronized");
        Ok(PreparationAdmission {
            runtime,
            preparation: PreparationPermit {
                _usage: usage,
                _admission: admission,
            },
        })
    }

    async fn wait_for_preparation(&self) -> Result<PreparationAdmission> {
        let runtime = self.begin_admission(false)?;
        let admission = Arc::clone(&self.inner.preparation_admission)
            .acquire_owned()
            .await
            .expect("the Runtime never closes preparation admission");
        let usage = self
            .inner
            .resources
            .preparations
            .try_reserve(1)
            .expect("preparation semaphore and resource ledger stay synchronized");
        Ok(PreparationAdmission {
            runtime,
            preparation: PreparationPermit {
                _usage: usage,
                _admission: admission,
            },
        })
    }

    pub(super) fn begin_plugin_preparation(&self) -> Result<PluginPreparation> {
        let admission = self.begin_preparation()?;
        let fiber_reservation = FiberReservation::reserve(&self.inner.resources)?;
        let attempt_reservations = self.reserve_attempt_resources()?;
        Ok(PluginPreparation {
            admission,
            fiber_reservation,
            attempt_reservations,
        })
    }

    pub(super) fn begin_attempt_preparation(
        &self,
    ) -> Result<(PreparationAdmission, AttemptReservations)> {
        let admission = self.begin_preparation()?;
        let reservations = self.reserve_attempt_resources()?;
        Ok((admission, reservations))
    }

    pub(super) async fn wait_for_attempt_preparation(
        &self,
    ) -> Result<(PreparationAdmission, AttemptReservations)> {
        let admission = self.wait_for_preparation().await?;
        let reservations = self.reserve_attempt_resources()?;
        Ok((admission, reservations))
    }

    fn reserve_attempt_resources(&self) -> Result<AttemptReservations> {
        let payloads = &self.inner.limits.payloads;
        let topology = &self.inner.limits.topology;
        let maximum_requirement_bytes = maximum_requirement_bytes(
            topology.maximum_requirements_per_fiber,
            payloads.maximum_identifier_bytes,
        )?;
        let pessimistic_retained = payloads
            .maximum_config_bytes
            .checked_add(payloads.maximum_prepared_state_bytes)
            .and_then(|bytes| bytes.checked_add(maximum_requirement_bytes))
            .ok_or_else(|| MetaError::InvalidInput("plugin payload limits overflow".to_owned()))?;
        AttemptReservations::reserve(
            &self.inner.resources,
            pessimistic_retained,
            topology.maximum_requirements_per_fiber,
        )
    }

    fn retain_desired_config(
        &self,
        desired: configuration::OwnedJsonValue,
        revision: u64,
    ) -> Result<DesiredConfig> {
        let desired_bytes = Self::validate_config(desired.as_value(), &self.inner.limits.payloads)?;
        let reservation = self
            .inner
            .resources
            .retained_plugin_bytes
            .try_reserve(desired_bytes)
            .ok_or(MetaError::CapacityExhausted {
                resource: "retained plugin bytes",
            })?;
        Ok(DesiredConfig {
            revision,
            value: Arc::new(RetainedConfig::new_validated(
                desired.into_inner(),
                reservation,
            )),
        })
    }

    pub(super) fn prepare_admitted(
        &self,
        identity: FactoryIdentity,
        update_mode: UpdateMode,
        factory: RetainedFactory,
        config: configuration::OwnedJsonValue,
        preparation: PluginPreparation,
    ) -> Result<PreparedPlugin> {
        let payloads = &self.inner.limits.payloads;
        let PluginPreparation {
            admission,
            mut fiber_reservation,
            attempt_reservations,
        } = preparation;
        let (runtime_admission, _preparation) = admission.into_parts();
        let desired = self.retain_desired_config(config, 1)?;
        validate_factory_identity(&identity, payloads.maximum_identifier_bytes)?;
        fiber_reservation
            .reserve_identity(&self.inner.resources, factory_identity_bytes(&identity)?)?;
        let attempt = self.prepare_attempt(
            &factory,
            &desired.value,
            desired.revision,
            attempt_reservations,
        )?;
        Ok(PreparedPlugin {
            runtime: Arc::downgrade(&self.inner),
            admission: runtime_admission,
            identity,
            update_mode,
            factory,
            desired,
            attempt,
            fiber_reservation,
        })
    }

    pub(super) fn prepare_attempt_admitted(
        &self,
        factory: &RetainedFactory,
        desired: configuration::OwnedJsonValue,
        desired_revision: u64,
        admission: PreparationAdmission,
        reservations: AttemptReservations,
    ) -> Result<(DesiredConfig, PreparedAttempt)> {
        let (runtime_admission, _preparation) = admission.into_parts();
        let desired = self.retain_desired_config(desired, desired_revision)?;
        let attempt =
            self.prepare_attempt(factory, &desired.value, desired_revision, reservations)?;
        drop(runtime_admission);
        Ok((desired, attempt))
    }

    pub(super) fn prepare_retained_attempt_admitted(
        &self,
        factory: &RetainedFactory,
        desired: &Arc<RetainedConfig>,
        desired_revision: u64,
        admission: PreparationAdmission,
        reservations: AttemptReservations,
    ) -> Result<PreparedAttempt> {
        let (runtime_admission, _preparation) = admission.into_parts();
        let attempt = self.prepare_attempt(factory, desired, desired_revision, reservations)?;
        drop(runtime_admission);
        Ok(attempt)
    }

    fn prepare_attempt(
        &self,
        factory: &RetainedFactory,
        desired: &Arc<RetainedConfig>,
        desired_revision: u64,
        mut reservations: AttemptReservations,
    ) -> Result<PreparedAttempt> {
        let payloads = &self.inner.limits.payloads;
        let topology = &self.inner.limits.topology;
        let normalized = Self::normalize_config(factory, desired, payloads)?;
        let requirement_bytes = validate_requirements(
            &normalized.requirements,
            topology.maximum_requirements_per_fiber,
            payloads.maximum_identifier_bytes,
        )?;
        let local_requirement_bytes = validate_local_requirements(
            &normalized.local_requirements,
            topology.maximum_requirements_per_fiber,
            payloads.maximum_identifier_bytes,
        )?;
        let requirement_count = normalized
            .requirements
            .len()
            .checked_add(normalized.local_requirements.len())
            .ok_or(MetaError::InvalidInput(
                "prepared activation has too many requirements".to_owned(),
            ))?;
        if requirement_count > topology.maximum_requirements_per_fiber {
            return Err(MetaError::InvalidInput(
                "prepared activation has too many requirements".to_owned(),
            ));
        }
        let state_bytes = normalized
            .state
            .as_ref()
            .map_or(0, PreparedState::retained_bytes);
        if state_bytes > payloads.maximum_prepared_state_bytes {
            return Err(MetaError::PayloadTooLarge {
                maximum: payloads.maximum_prepared_state_bytes,
            });
        }
        let non_config_bytes = requirement_bytes
            .checked_add(local_requirement_bytes)
            .ok_or(MetaError::CapacityExhausted {
                resource: "retained plugin bytes",
            })?
            .checked_add(state_bytes)
            .ok_or(MetaError::CapacityExhausted {
                resource: "retained plugin bytes",
            })?;
        let retained_bytes = non_config_bytes
            .checked_add(normalized.encoded_bytes)
            .ok_or(MetaError::CapacityExhausted {
                resource: "retained plugin bytes",
            })?;
        reservations.shrink_to(retained_bytes, requirement_count);
        let config_reservation = reservations.split_retained_config(non_config_bytes);
        Ok(PreparedAttempt {
            id: self.next_attempt_id()?,
            desired_revision,
            requirements: normalized.requirements.into(),
            local_requirements: normalized.local_requirements.into(),
            config: Arc::new(RetainedConfig::new_validated(
                normalized.value.into_inner(),
                config_reservation,
            )),
            state: normalized.state,
            consumed: false,
            _reservations: reservations,
        })
    }
}

fn validate_factory_identity(identity: &FactoryIdentity, maximum: usize) -> Result<()> {
    let valid = match identity {
        FactoryIdentity::Linked { plugin, revision } => {
            !plugin.as_str().is_empty()
                && plugin.as_str().len() <= maximum
                && !revision.is_empty()
                && revision.len() <= maximum
        }
        FactoryIdentity::Native { plugin, sha256 } => {
            !plugin.as_str().is_empty()
                && plugin.as_str().len() <= maximum
                && sha256.len() == 64
                && sha256
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
        }
    };
    if !valid {
        return Err(MetaError::InvalidInput(
            "plugin factory provenance is empty, malformed, or exceeds its byte limit".to_owned(),
        ));
    }
    Ok(())
}

fn factory_identity_bytes(identity: &FactoryIdentity) -> Result<usize> {
    let (first, second) = match identity {
        FactoryIdentity::Linked { plugin, revision } => (plugin.as_str().len(), revision.len()),
        FactoryIdentity::Native { plugin, sha256 } => (plugin.as_str().len(), sha256.len()),
    };
    first
        .checked_add(second)
        .ok_or(MetaError::CapacityExhausted {
            resource: "retained plugin bytes",
        })
}

fn maximum_requirement_bytes(count: usize, maximum_identifier_bytes: usize) -> Result<usize> {
    let per_requirement = maximum_identifier_bytes
        .checked_mul(2)
        .and_then(|bytes| bytes.checked_add(std::mem::size_of::<ContractVersion>()))
        .ok_or_else(|| MetaError::InvalidInput("requirement payload limits overflow".to_owned()))?;
    count
        .checked_mul(per_requirement)
        .ok_or_else(|| MetaError::InvalidInput("requirement payload limits overflow".to_owned()))
}

fn validate_requirements(
    requirements: &[Requirement],
    maximum_count: usize,
    maximum_identifier_bytes: usize,
) -> Result<usize> {
    if requirements.len() > maximum_count {
        return Err(MetaError::InvalidInput(
            "prepared activation has too many requirements".to_owned(),
        ));
    }
    let mut keys = BTreeSet::new();
    let mut retained_bytes = 0_usize;
    for requirement in requirements {
        validate_service_identity(
            &requirement.key,
            &requirement.contract,
            maximum_identifier_bytes,
        )?;
        if !keys.insert(&requirement.key) {
            return Err(MetaError::InvalidInput(format!(
                "prepared activation requires service {} more than once",
                requirement.key
            )));
        }
        retained_bytes = retained_bytes
            .checked_add(requirement.key.as_str().len())
            .and_then(|bytes| bytes.checked_add(requirement.contract.as_str().len()))
            .and_then(|bytes| bytes.checked_add(std::mem::size_of::<ContractVersion>()))
            .ok_or(MetaError::CapacityExhausted {
                resource: "retained plugin bytes",
            })?;
    }
    Ok(retained_bytes)
}

fn validate_local_requirements(
    requirements: &[LocalRequirement],
    maximum_count: usize,
    maximum_identifier_bytes: usize,
) -> Result<usize> {
    if requirements.len() > maximum_count {
        return Err(MetaError::InvalidInput(
            "prepared activation has too many requirements".to_owned(),
        ));
    }
    let mut contracts = BTreeSet::new();
    let mut retained_bytes = 0_usize;
    for requirement in requirements {
        if requirement.key.as_str().len() > maximum_identifier_bytes {
            return Err(MetaError::InvalidInput(
                "prepared Local contract identifier exceeds the configured byte limit".to_owned(),
            ));
        }
        if !contracts.insert(requirement.contract) {
            return Err(MetaError::InvalidInput(format!(
                "prepared activation requires Local contract {} more than once",
                requirement.key
            )));
        }
        retained_bytes = retained_bytes
            .checked_add(requirement.key.as_str().len())
            .ok_or(MetaError::CapacityExhausted {
                resource: "retained plugin bytes",
            })?;
    }
    Ok(retained_bytes)
}

fn validate_service_identity(
    key: &ServiceKey,
    contract: &ContractId,
    maximum: usize,
) -> Result<()> {
    if key.as_str().len() > maximum || contract.as_str().len() > maximum {
        return Err(MetaError::InvalidInput(
            "prepared service identifier exceeds the configured byte limit".to_owned(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::PreparedActivation;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[derive(Debug)]
    struct CountedState(Arc<AtomicUsize>);

    impl Drop for CountedState {
        fn drop(&mut self) {
            self.0.fetch_add(1, Ordering::AcqRel);
        }
    }

    struct CountingFactory {
        prepare_calls: Arc<AtomicUsize>,
        state_drops: Arc<AtomicUsize>,
        state_bytes: usize,
    }

    impl fmt::Debug for CountingFactory {
        fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter.debug_struct("CountingFactory").finish()
        }
    }

    #[async_trait::async_trait]
    impl PluginFactory for CountingFactory {
        fn prepare(&self, desired: &ConfigValue) -> Result<PreparedActivation> {
            self.prepare_calls.fetch_add(1, Ordering::AcqRel);
            Ok(PreparedActivation::with_state(
                desired.clone(),
                CountedState(Arc::clone(&self.state_drops)),
                self.state_bytes,
            )
            .requiring(Requirement::new("x", "y", ContractVersion(1))))
        }

        async fn activate(&self, _plan: ActivationPlan) -> Result<()> {
            Ok(())
        }
    }

    struct TakingStateFactory {
        runtime: Weak<RuntimeInner>,
        retained_during_activation: Arc<AtomicUsize>,
    }

    impl fmt::Debug for TakingStateFactory {
        fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter.debug_struct("TakingStateFactory").finish()
        }
    }

    #[async_trait::async_trait]
    impl PluginFactory for TakingStateFactory {
        fn prepare(&self, desired: &ConfigValue) -> Result<PreparedActivation> {
            Ok(PreparedActivation::with_state(
                desired.clone(),
                [0_u8; 23],
                23,
            ))
        }

        async fn activate(&self, mut plan: ActivationPlan) -> Result<()> {
            let _state = plan.take_state::<[u8; 23]>()?;
            let runtime = self
                .runtime
                .upgrade()
                .expect("activation retains its Runtime");
            self.retained_during_activation.store(
                runtime.resources.snapshot().retained_plugin_bytes.current,
                Ordering::Release,
            );
            Ok(())
        }
    }

    fn counting_factory(
        state_bytes: usize,
    ) -> (Arc<CountingFactory>, Arc<AtomicUsize>, Arc<AtomicUsize>) {
        let prepare_calls = Arc::new(AtomicUsize::new(0));
        let state_drops = Arc::new(AtomicUsize::new(0));
        (
            Arc::new(CountingFactory {
                prepare_calls: Arc::clone(&prepare_calls),
                state_drops: Arc::clone(&state_drops),
                state_bytes,
            }),
            prepare_calls,
            state_drops,
        )
    }

    #[test]
    fn resolved_identity_attempt_and_state_have_exact_independent_accounting() {
        let runtime = Runtime::default();
        let (factory, prepare_calls, state_drops) = counting_factory(5);
        let proof = runtime
            .prepare(
                ResolvedFactory::linked("counted", "1", UpdateMode::Replayable, factory),
                ConfigValue::Null,
            )
            .expect("bounded preparation succeeds");

        assert_eq!(prepare_calls.load(Ordering::Acquire), 1);
        let config_bytes = serde_json::to_vec(&ConfigValue::Null).unwrap().len();
        let identity_bytes = "counted".len() + "1".len();
        let requirement_bytes = "x".len() + "y".len() + std::mem::size_of::<ContractVersion>();
        let snapshot = runtime.resource_snapshot();
        assert_eq!(snapshot.fibers.current, 1);
        assert_eq!(snapshot.dependency_edges.current, 1);
        assert_eq!(
            snapshot.retained_plugin_bytes.current,
            identity_bytes + 2 * config_bytes + requirement_bytes + 5
        );

        drop(proof);
        let released = runtime.resource_snapshot();
        assert_eq!(released.fibers.current, 0);
        assert_eq!(released.dependency_edges.current, 0);
        assert_eq!(released.retained_plugin_bytes.current, 0);
        assert_eq!(state_drops.load(Ordering::Acquire), 1);
    }

    #[test]
    fn oversized_declared_state_releases_every_preparation_resource() {
        let mut limits = RuntimeLimits::default();
        limits.payloads.maximum_prepared_state_bytes = 1;
        let runtime = Runtime::new(limits).unwrap();
        let (factory, prepare_calls, state_drops) = counting_factory(2);

        assert_eq!(
            runtime
                .prepare(
                    ResolvedFactory::linked("counted", "1", UpdateMode::Replayable, factory),
                    ConfigValue::Null,
                )
                .unwrap_err(),
            MetaError::PayloadTooLarge { maximum: 1 }
        );
        assert_eq!(prepare_calls.load(Ordering::Acquire), 1);
        assert_eq!(state_drops.load(Ordering::Acquire), 1);
        let released = runtime.resource_snapshot();
        assert_eq!(released.preparations.current, 0);
        assert_eq!(released.fibers.current, 0);
        assert_eq!(released.dependency_edges.current, 0);
        assert_eq!(released.retained_plugin_bytes.current, 0);
    }

    #[tokio::test]
    async fn taken_state_charge_remains_until_attempt_retires() {
        let runtime = Runtime::default();
        let retained_during_activation = Arc::new(AtomicUsize::new(0));
        let factory = Arc::new(TakingStateFactory {
            runtime: Arc::downgrade(&runtime.inner),
            retained_during_activation: Arc::clone(&retained_during_activation),
        });
        let fiber = runtime
            .root()
            .apply(
                ResolvedFactory::linked("taking-state", "1", UpdateMode::Replayable, factory),
                ConfigValue::Null,
            )
            .await
            .expect("state-taking activation succeeds");

        let config_bytes = serde_json::to_vec(&ConfigValue::Null).unwrap().len();
        let expected = "taking-state".len() + "1".len() + 2 * config_bytes + 23;
        assert_eq!(retained_during_activation.load(Ordering::Acquire), expected);
        assert_eq!(
            runtime.resource_snapshot().retained_plugin_bytes.current,
            expected
        );

        assert!(fiber.dispose().await.is_clean());
        assert_eq!(runtime.resource_snapshot().retained_plugin_bytes.current, 0);
        assert!(runtime.shutdown().await.is_complete());
    }

    #[tokio::test]
    async fn reprepare_reuses_the_resolver_owned_factory_identity() {
        let runtime = Runtime::default();
        let (factory, prepare_calls, _state_drops) = counting_factory(0);
        let fiber = runtime
            .root()
            .apply(
                ResolvedFactory::linked("counted", "1", UpdateMode::Replayable, factory),
                ConfigValue::Null,
            )
            .await
            .expect("initial apply is admitted");
        assert!(matches!(fiber.snapshot().state, FiberState::Pending(_)));
        fiber
            .reconfigure(serde_json::json!({"desired": 2}))
            .await
            .expect("replacement preparation settles");

        assert_eq!(prepare_calls.load(Ordering::Acquire), 2);
        assert!(fiber.dispose().await.is_clean());
        assert!(runtime.shutdown().await.is_complete());
    }
}
