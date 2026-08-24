#![allow(clippy::wildcard_imports)] // This is one implementation partition of runtime.

use super::*;
use crate::service::LeaseGuard;
use crate::{Provision, Requirement};
use limits::{ResourceReservation, RuntimeResources};
use std::ops::Deref;

/// Opaque Runtime-bound proof that descriptor validation, resource reservation,
/// and configuration normalization completed successfully exactly once.
pub struct PreparedPlugin {
    pub(super) runtime: Weak<RuntimeInner>,
    pub(super) admission: LeaseGuard,
    pub(super) factory: Arc<dyn PluginFactory>,
    pub(super) descriptor: Arc<PreparedDescriptor>,
    pub(super) config: Arc<RetainedConfig>,
    pub(super) reservations: PreparedReservations,
}

impl fmt::Debug for PreparedPlugin {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PreparedPlugin")
            .field("descriptor", &self.descriptor.value)
            .finish_non_exhaustive()
    }
}

pub(super) struct PreparedDescriptor {
    value: PluginDescriptor,
    encoded_bytes: usize,
    requirements: BTreeMap<ServiceKey, usize>,
    provisions: BTreeMap<ServiceKey, usize>,
}

impl PreparedDescriptor {
    fn new(value: PluginDescriptor, encoded_bytes: usize) -> Self {
        let requirements = value
            .requires
            .iter()
            .enumerate()
            .map(|(index, requirement)| (requirement.key.clone(), index))
            .collect();
        let provisions = value
            .provides
            .iter()
            .enumerate()
            .map(|(index, provision)| (provision.key.clone(), index))
            .collect();
        Self {
            value,
            encoded_bytes,
            requirements,
            provisions,
        }
    }

    pub(super) fn encoded_bytes(&self) -> usize {
        self.encoded_bytes
    }

    pub(super) fn requirement(&self, key: &ServiceKey) -> Option<&Requirement> {
        self.requirements
            .get(key)
            .map(|index| &self.value.requires[*index])
    }

    pub(super) fn provision(&self, key: &ServiceKey) -> Option<&Provision> {
        self.provisions
            .get(key)
            .map(|index| &self.value.provides[*index])
    }

    pub(super) fn required_services(&self) -> impl Iterator<Item = &ServiceKey> {
        self.value
            .requires
            .iter()
            .map(|requirement| &requirement.key)
    }

    pub(super) fn provided_services(&self) -> impl Iterator<Item = &ServiceKey> {
        self.value.provides.iter().map(|provision| &provision.key)
    }
}

impl Deref for PreparedDescriptor {
    type Target = PluginDescriptor;

    fn deref(&self) -> &Self::Target {
        &self.value
    }
}

pub(super) struct PreparedReservations {
    _fiber: ResourceReservation,
    retained_plugin_bytes: ResourceReservation,
    service_declarations: Option<ResourceReservation>,
    dependency_edges: Option<ResourceReservation>,
}

pub(super) struct PreparationAdmission {
    runtime: LeaseGuard,
    preparation: ResourceReservation,
}

pub(super) struct PluginPreparation {
    admission: PreparationAdmission,
    reservations: PreparedReservations,
}

impl PreparationAdmission {
    pub(super) fn into_parts(self) -> (LeaseGuard, ResourceReservation) {
        (self.runtime, self.preparation)
    }
}

impl PreparedReservations {
    fn reserve_base(resources: &RuntimeResources, retained_bytes: usize) -> Result<Self> {
        let fiber = resources
            .fibers
            .try_reserve(1)
            .ok_or(MetaError::CapacityExhausted { resource: "fibers" })?;
        let retained_plugin_bytes = resources
            .retained_plugin_bytes
            .try_reserve(retained_bytes)
            .ok_or(MetaError::CapacityExhausted {
                resource: "retained plugin bytes",
            })?;
        Ok(Self {
            _fiber: fiber,
            retained_plugin_bytes,
            service_declarations: None,
            dependency_edges: None,
        })
    }

    fn reserve_descriptor_topology(
        &mut self,
        resources: &RuntimeResources,
        declaration_count: usize,
        dependency_count: usize,
    ) -> Result<()> {
        debug_assert!(
            self.service_declarations.is_none() && self.dependency_edges.is_none(),
            "descriptor topology may only be reserved once"
        );
        let service_declarations = resources
            .service_declarations
            .try_reserve(declaration_count)
            .ok_or(MetaError::CapacityExhausted {
                resource: "service declarations",
            })?;
        let dependency_edges = resources
            .dependency_edges
            .try_reserve(dependency_count)
            .ok_or(MetaError::CapacityExhausted {
                resource: "dependency edges",
            })?;
        self.service_declarations = Some(service_declarations);
        self.dependency_edges = Some(dependency_edges);
        Ok(())
    }

    fn shrink_retained_to(&mut self, retained_bytes: usize) {
        self.retained_plugin_bytes.shrink_to(retained_bytes);
    }

    fn split_retained_config(&mut self, descriptor_bytes: usize) -> ResourceReservation {
        self.retained_plugin_bytes.split_off(descriptor_bytes)
    }
}

struct DescriptorFacts {
    declaration_count: usize,
    dependency_count: usize,
}

impl Runtime {
    /// Validates a descriptor, reserves retained resources, and normalizes
    /// bounded configuration exactly once.
    ///
    /// The returned proof belongs to this Runtime and can be consumed by
    /// [`Context::apply_prepared`] without invoking factory normalization again.
    pub fn prepare(
        &self,
        factory: Arc<dyn PluginFactory>,
        config: ConfigValue,
    ) -> Result<PreparedPlugin> {
        let config = configuration::OwnedJsonValue::new(config);
        let preparation = self.begin_plugin_preparation()?;
        self.prepare_admitted(factory, config, preparation)
    }

    pub(super) fn begin_preparation(&self) -> Result<PreparationAdmission> {
        let runtime = self.begin_admission(false)?;
        let preparation =
            self.inner
                .resources
                .preparations
                .try_reserve(1)
                .ok_or(MetaError::Busy {
                    operation: "plugin preparation",
                })?;
        Ok(PreparationAdmission {
            runtime,
            preparation,
        })
    }

    pub(super) fn begin_plugin_preparation(&self) -> Result<PluginPreparation> {
        let preparation = self.begin_preparation()?;
        let payloads = &self.inner.limits.payloads;
        let pessimistic_retained = payloads
            .maximum_descriptor_bytes
            .checked_add(payloads.maximum_config_bytes)
            .expect("validated Runtime limits prevent plugin payload overflow");
        let reservations =
            PreparedReservations::reserve_base(&self.inner.resources, pessimistic_retained)?;
        Ok(PluginPreparation {
            admission: preparation,
            reservations,
        })
    }

    pub(super) fn prepare_admitted(
        &self,
        factory: Arc<dyn PluginFactory>,
        config: configuration::OwnedJsonValue,
        preparation: PluginPreparation,
    ) -> Result<PreparedPlugin> {
        let payloads = &self.inner.limits.payloads;
        let topology = &self.inner.limits.topology;
        let PluginPreparation {
            admission,
            mut reservations,
        } = preparation;
        let (runtime_admission, _preparation) = admission.into_parts();
        let descriptor = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let descriptor = factory.descriptor();
            let facts = validate_descriptor_metadata(descriptor, topology, payloads)?;
            reservations.reserve_descriptor_topology(
                &self.inner.resources,
                facts.declaration_count,
                facts.dependency_count,
            )?;
            let encoded_bytes = configuration::encoded_json_size_bounded(
                descriptor,
                payloads.maximum_descriptor_bytes,
            )
            .map_err(|error| MetaError::InvalidInput(error.to_string()))?;
            let retained_through_normalization = encoded_bytes
                .checked_add(payloads.maximum_config_bytes)
                .expect("bounded descriptor plus validated config limit cannot overflow");
            reservations.shrink_retained_to(retained_through_normalization);
            Ok::<_, MetaError>(Arc::new(PreparedDescriptor::new(
                descriptor.clone(),
                encoded_bytes,
            )))
        }))
        .map_err(|_| MetaError::Activation("plugin descriptor validation panicked".to_owned()))??;
        let normalized = Self::normalize_config(&factory, config, payloads)?;
        let retained_bytes = descriptor
            .encoded_bytes()
            .checked_add(normalized.encoded_bytes)
            .ok_or(MetaError::CapacityExhausted {
                resource: "retained plugin bytes",
            })?;
        reservations.shrink_retained_to(retained_bytes);
        let config_reservation = reservations.split_retained_config(descriptor.encoded_bytes());
        Ok(PreparedPlugin {
            runtime: Arc::downgrade(&self.inner),
            admission: runtime_admission,
            factory,
            descriptor,
            config: Arc::new(RetainedConfig::new(normalized.value, config_reservation)),
            reservations,
        })
    }
}

fn validate_descriptor_metadata(
    descriptor: &PluginDescriptor,
    topology: &TopologyLimits,
    payloads: &PayloadLimits,
) -> Result<DescriptorFacts> {
    validate_factory_identity(&descriptor.identity, payloads.maximum_identifier_bytes)?;
    if descriptor.requires.len() > topology.maximum_requirements_per_fiber {
        return Err(MetaError::InvalidInput(
            "plugin descriptor has too many requirements".to_owned(),
        ));
    }
    if descriptor.provides.len() > topology.maximum_provisions_per_fiber {
        return Err(MetaError::InvalidInput(
            "plugin descriptor has too many provisions".to_owned(),
        ));
    }
    let declaration_count = descriptor
        .requires
        .len()
        .checked_add(descriptor.provides.len())
        .ok_or_else(|| MetaError::InvalidInput("descriptor count overflow".to_owned()))?;
    validate_descriptor_json_shape(declaration_count, payloads)?;
    let mut requirements = BTreeSet::new();
    for requirement in &descriptor.requires {
        validate_service_identity(
            &requirement.key,
            &requirement.contract,
            payloads.maximum_identifier_bytes,
        )?;
        if !requirements.insert(&requirement.key) {
            return Err(MetaError::InvalidInput(format!(
                "factory {} declares requirement {} more than once",
                descriptor.identity, requirement.key
            )));
        }
    }
    let mut provisions = BTreeSet::new();
    for provision in &descriptor.provides {
        validate_service_identity(
            &provision.key,
            &provision.contract,
            payloads.maximum_identifier_bytes,
        )?;
        if !provisions.insert(&provision.key) {
            return Err(MetaError::InvalidInput(format!(
                "factory {} declares provision {} more than once",
                descriptor.identity, provision.key
            )));
        }
    }
    Ok(DescriptorFacts {
        declaration_count,
        dependency_count: descriptor.requires.len(),
    })
}

fn validate_descriptor_json_shape(
    declaration_count: usize,
    payloads: &PayloadLimits,
) -> Result<()> {
    // The tagged identity, root object, and two declaration arrays contain
    // seven nodes. Each typed requirement or provision adds its object and
    // three scalar fields. Nonempty declarations add one level below an array.
    let nodes = declaration_count
        .checked_mul(4)
        .and_then(|nodes| nodes.checked_add(7))
        .ok_or_else(|| MetaError::InvalidInput("descriptor JSON node count overflow".to_owned()))?;
    let depth = if declaration_count == 0 { 3 } else { 4 };
    if depth > payloads.maximum_json_depth || nodes > payloads.maximum_json_nodes {
        return Err(MetaError::InvalidInput(
            "plugin descriptor exceeds the configured JSON shape limits".to_owned(),
        ));
    }
    Ok(())
}

fn validate_factory_identity(identity: &FactoryIdentity, maximum: usize) -> Result<()> {
    let valid = match identity {
        FactoryIdentity::Builtin { name, revision } => {
            name.len() <= maximum && revision.len() <= maximum
        }
        FactoryIdentity::Artifact { plugin, sha256 } => {
            plugin.len() <= maximum && sha256.len() <= maximum
        }
    };
    if !valid {
        return Err(MetaError::InvalidInput(
            "plugin descriptor identifier exceeds the configured byte limit".to_owned(),
        ));
    }
    Ok(())
}

fn validate_service_identity(
    key: &ServiceKey,
    contract: &ContractId,
    maximum: usize,
) -> Result<()> {
    if key.as_str().len() > maximum || contract.as_str().len() > maximum {
        return Err(MetaError::InvalidInput(
            "service descriptor identifier exceeds the configured byte limit".to_owned(),
        ));
    }
    Ok(())
}
