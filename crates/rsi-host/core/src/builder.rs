use crate::host::{LinkedCatalog, PROFILE_PLUGIN_ID};
use crate::{
    Host, HostError, HostPaths, ProfileControlContract, ProfileFragment, ProfileLimits,
    ProfilePatch, Result,
};
use rsi_meta::{
    ActivationPlan, ConfigValue, LocalContract, LocalContractKey, LocalEvent, LocalEventKey,
    PluginFactory, PluginId, PreparedActivation, Runtime, RuntimeLimits, UpdateMode,
};
use rsi_meta_profile::ProfileEnvironment;
use std::any::{TypeId, type_name};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;

/// Explicit bounds for Host-owned catalog inputs and delegated Profile work.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HostLimits {
    /// Bounds enforced by `rsi-meta-profile`.
    pub profile: ProfileLimits,
    /// Maximum linked factory registrations.
    pub maximum_linked_plugins: usize,
    /// Maximum immutable linked fragments.
    pub maximum_fragments: usize,
    /// Maximum registered Local contract markers.
    pub maximum_local_contracts: usize,
    /// Maximum registered Local event markers.
    pub maximum_local_events: usize,
    /// Maximum bytes in a Host catalog identifier or linked revision.
    pub maximum_identifier_bytes: usize,
}

impl Default for HostLimits {
    fn default() -> Self {
        Self {
            profile: ProfileLimits::default(),
            maximum_linked_plugins: 4_096,
            maximum_fragments: 256,
            maximum_local_contracts: 4_096,
            maximum_local_events: 4_096,
            maximum_identifier_bytes: 256,
        }
    }
}

/// Freezes all generic Host composition inputs before Runtime creation.
pub struct HostBuilder {
    paths: HostPaths,
    platform: String,
    defines: BTreeMap<String, ConfigValue>,
    limits: HostLimits,
    runtime_limits: RuntimeLimits,
    linked: BTreeMap<PluginId, LinkedRegistration>,
    local_contract_keys: BTreeMap<LocalContractKey, TypeId>,
    local_contract_types: HashMap<TypeId, &'static str>,
    local_event_keys: BTreeMap<LocalEventKey, TypeId>,
    local_event_types: HashMap<TypeId, &'static str>,
    fragments: Vec<ProfileFragment>,
    fragment_ids: HashSet<String>,
    launch_patches: Vec<ProfilePatch>,
}

impl std::fmt::Debug for HostBuilder {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("HostBuilder")
            .field("paths", &self.paths)
            .field("platform", &self.platform)
            .field("defines", &self.defines.keys())
            .field("limits", &self.limits)
            .field("runtime_limits", &self.runtime_limits)
            .field("linked", &self.linked.keys())
            .field("fragments", &self.fragment_ids)
            .finish_non_exhaustive()
    }
}

#[derive(Clone, Debug)]
pub(crate) struct LinkedRegistration {
    pub(crate) revision: String,
    pub(crate) update_mode: UpdateMode,
    pub(crate) implementation: Arc<dyn PluginFactory>,
}

struct ContainedFactory {
    inner: Option<Arc<dyn PluginFactory>>,
}

impl std::fmt::Debug for ContainedFactory {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ContainedFactory")
            .finish_non_exhaustive()
    }
}

#[async_trait::async_trait]
impl PluginFactory for ContainedFactory {
    fn prepare(&self, desired: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        self.inner
            .as_ref()
            .expect("linked factory remains available until destruction")
            .prepare(desired)
    }

    async fn activate(&self, plan: ActivationPlan) -> rsi_meta::Result<()> {
        self.inner
            .as_ref()
            .expect("linked factory remains available until destruction")
            .activate(plan)
            .await
    }
}

impl Drop for ContainedFactory {
    fn drop(&mut self) {
        if let Some(inner) = self.inner.take() {
            drop_contained(inner);
        }
    }
}

impl HostBuilder {
    /// Creates a builder from explicit path authority and frozen target platform.
    pub fn new(paths: HostPaths) -> Self {
        Self {
            paths,
            platform: format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH),
            defines: BTreeMap::new(),
            limits: HostLimits::default(),
            runtime_limits: RuntimeLimits::default(),
            linked: BTreeMap::new(),
            local_contract_keys: BTreeMap::new(),
            local_contract_types: HashMap::new(),
            local_event_keys: BTreeMap::new(),
            local_event_types: HashMap::new(),
            fragments: Vec::new(),
            fragment_ids: HashSet::new(),
            launch_patches: Vec::new(),
        }
    }

    /// Replaces Host-owned input bounds.
    #[must_use]
    pub fn limits(mut self, limits: HostLimits) -> Self {
        self.limits = limits;
        self
    }

    /// Replaces the Meta Runtime policy created during build.
    #[must_use]
    pub fn runtime_limits(mut self, limits: RuntimeLimits) -> Self {
        self.runtime_limits = limits;
        self
    }

    /// Replaces the frozen platform value visible to pure Profile Rhai.
    pub fn platform(&mut self, platform: impl Into<String>) -> Result<&mut Self> {
        let platform = platform.into();
        validate_identifier("platform", &platform, self.limits.maximum_identifier_bytes)?;
        self.platform = platform;
        Ok(self)
    }

    /// Registers one frozen JSON-compatible value visible to pure Profile Rhai.
    pub fn define(&mut self, key: impl Into<String>, value: ConfigValue) -> Result<&mut Self> {
        let key = key.into();
        validate_identifier("define", &key, self.limits.maximum_identifier_bytes)?;
        if self.defines.contains_key(&key) {
            return Err(HostError::DuplicateDefine { key });
        }
        self.defines.insert(key, value);
        Ok(self)
    }

    /// Returns the explicit frozen path candidate.
    pub const fn paths(&self) -> &HostPaths {
        &self.paths
    }

    /// Registers one process-linked implementation without executing it.
    pub fn register_linked(
        &mut self,
        plugin: impl Into<PluginId>,
        revision: impl Into<String>,
        update_mode: UpdateMode,
        implementation: Arc<dyn PluginFactory>,
    ) -> Result<&mut Self> {
        validate_limits(&self.limits)?;
        if self.linked.len() >= self.limits.maximum_linked_plugins {
            return Err(HostError::CapacityExceeded {
                resource: "linked plugins",
                maximum: self.limits.maximum_linked_plugins,
            });
        }
        let plugin = plugin.into();
        validate_identifier(
            "plugin",
            plugin.as_str(),
            self.limits.maximum_identifier_bytes,
        )?;
        if plugin.as_str() == PROFILE_PLUGIN_ID || self.linked.contains_key(&plugin) {
            return Err(HostError::DuplicatePlugin { plugin });
        }
        let revision = revision.into();
        validate_identifier("revision", &revision, self.limits.maximum_identifier_bytes)?;
        let implementation: Arc<dyn PluginFactory> = Arc::new(ContainedFactory {
            inner: Some(implementation),
        });
        self.linked.insert(
            plugin,
            LinkedRegistration {
                revision,
                update_mode,
                implementation,
            },
        );
        Ok(self)
    }

    /// Registers one exact Rust Local contract marker for Profile naming.
    pub fn register_local_contract<C: LocalContract>(&mut self) -> Result<&mut Self> {
        if self.local_contract_keys.len() >= self.limits.maximum_local_contracts {
            return Err(HostError::CapacityExceeded {
                resource: "Local contracts",
                maximum: self.limits.maximum_local_contracts,
            });
        }
        let contract = TypeId::of::<C>();
        let key = LocalContractKey::new(C::KEY);
        if contract == TypeId::of::<ProfileControlContract>()
            || key.as_str() == ProfileControlContract::KEY
            || self.local_contract_types.contains_key(&contract)
        {
            return Err(HostError::DuplicateLocalContractType {
                type_name: type_name::<C>(),
            });
        }
        validate_identifier(
            "Local contract",
            key.as_str(),
            self.limits.maximum_identifier_bytes,
        )?;
        if self.local_contract_keys.contains_key(&key) {
            return Err(HostError::DuplicateLocalContractKey { key });
        }
        self.local_contract_keys.insert(key, contract);
        self.local_contract_types.insert(contract, type_name::<C>());
        Ok(self)
    }

    /// Registers one exact Rust Local event marker for Profile naming.
    pub fn register_local_event<E: LocalEvent>(&mut self) -> Result<&mut Self> {
        if self.local_event_keys.len() >= self.limits.maximum_local_events {
            return Err(HostError::CapacityExceeded {
                resource: "Local events",
                maximum: self.limits.maximum_local_events,
            });
        }
        let event = TypeId::of::<E>();
        if self.local_event_types.contains_key(&event) {
            return Err(HostError::DuplicateLocalEventType {
                type_name: type_name::<E>(),
            });
        }
        let key = LocalEventKey::new(E::KEY);
        validate_identifier(
            "Local event",
            key.as_str(),
            self.limits.maximum_identifier_bytes,
        )?;
        if self.local_event_keys.contains_key(&key) {
            return Err(HostError::DuplicateLocalEventKey { key });
        }
        self.local_event_keys.insert(key, event);
        self.local_event_types.insert(event, type_name::<E>());
        Ok(self)
    }

    /// Appends one immutable linked fragment after validating its key.
    pub fn register_fragment(&mut self, fragment: ProfileFragment) -> Result<&mut Self> {
        if self.fragments.len() >= self.limits.maximum_fragments {
            return Err(HostError::CapacityExceeded {
                resource: "Profile fragments",
                maximum: self.limits.maximum_fragments,
            });
        }
        validate_identifier(
            "fragment",
            fragment.id(),
            self.limits.maximum_identifier_bytes,
        )?;
        if !self.fragment_ids.insert(fragment.id().to_owned()) {
            return Err(HostError::DuplicateFragment {
                fragment: fragment.id().to_owned(),
            });
        }
        self.fragments.push(fragment);
        Ok(self)
    }

    /// Appends one immutable launch patch after every file source step.
    pub fn register_launch_patch(&mut self, patch: ProfilePatch) -> Result<&mut Self> {
        if self.launch_patches.len() >= self.limits.profile.maximum_steps {
            return Err(HostError::CapacityExceeded {
                resource: "launch patches",
                maximum: self.limits.profile.maximum_steps,
            });
        }
        self.launch_patches.push(patch);
        Ok(self)
    }

    /// Validates and freezes all inputs, then creates one generic Host.
    pub fn build(self) -> Result<Host> {
        validate_limits(&self.limits)?;
        self.limits.profile.validate()?;
        validate_collection(
            "linked plugins",
            self.linked.len(),
            self.limits.maximum_linked_plugins,
        )?;
        validate_collection(
            "Profile fragments",
            self.fragments.len(),
            self.limits.maximum_fragments,
        )?;
        validate_collection(
            "Local contracts",
            self.local_contract_keys.len(),
            self.limits.maximum_local_contracts,
        )?;
        validate_collection(
            "Local events",
            self.local_event_keys.len(),
            self.limits.maximum_local_events,
        )?;
        validate_collection(
            "launch patches",
            self.launch_patches.len(),
            self.limits.profile.maximum_steps,
        )?;
        validate_identifier(
            "platform",
            &self.platform,
            self.limits.maximum_identifier_bytes,
        )?;
        for key in self.defines.keys() {
            validate_identifier("define", key, self.limits.maximum_identifier_bytes)?;
        }
        for (plugin, registration) in &self.linked {
            validate_identifier(
                "plugin",
                plugin.as_str(),
                self.limits.maximum_identifier_bytes,
            )?;
            validate_identifier(
                "revision",
                &registration.revision,
                self.limits.maximum_identifier_bytes,
            )?;
        }
        for key in self.local_contract_keys.keys() {
            validate_identifier(
                "Local contract",
                key.as_str(),
                self.limits.maximum_identifier_bytes,
            )?;
        }
        for key in self.local_event_keys.keys() {
            validate_identifier(
                "Local event",
                key.as_str(),
                self.limits.maximum_identifier_bytes,
            )?;
        }
        for fragment in &self.fragments {
            validate_identifier(
                "fragment",
                fragment.id(),
                self.limits.maximum_identifier_bytes,
            )?;
            validate_identifier(
                "fragment",
                fragment.id(),
                self.limits.profile.maximum_identifier_bytes,
            )?;
        }
        ProfileEnvironment::new(
            self.paths.config(),
            self.paths.state(),
            self.paths.cache(),
            self.platform.clone(),
            self.defines.clone(),
        )?
        .validate(&self.limits.profile)?;
        let runtime = Runtime::new(self.runtime_limits)?;
        Ok(Host::new(
            self.paths,
            self.platform,
            self.defines,
            self.limits,
            runtime,
            LinkedCatalog {
                linked: self.linked,
                fragments: self.fragments,
                local_contracts: self.local_contract_keys,
                local_events: self.local_event_keys,
                launch_patches: self.launch_patches,
            },
        ))
    }
}

fn drop_contained<T>(value: T) {
    if let Err(payload) = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| drop(value)))
        && let Err(payload) =
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| drop(payload)))
    {
        std::mem::forget(payload);
    }
}

pub(crate) fn validate_identifier(kind: &'static str, value: &str, maximum: usize) -> Result<()> {
    if value.is_empty() || value.len() > maximum {
        Err(HostError::InvalidIdentifier { kind, maximum })
    } else {
        Ok(())
    }
}

fn validate_limits(limits: &HostLimits) -> Result<()> {
    for (resource, value) in [
        ("linked plugins", limits.maximum_linked_plugins),
        ("Profile fragments", limits.maximum_fragments),
        ("Local contracts", limits.maximum_local_contracts),
        ("Local events", limits.maximum_local_events),
        ("identifier bytes", limits.maximum_identifier_bytes),
    ] {
        if value == 0 {
            return Err(HostError::CapacityExceeded {
                resource,
                maximum: 0,
            });
        }
    }
    Ok(())
}

fn validate_collection(resource: &'static str, actual: usize, maximum: usize) -> Result<()> {
    if actual > maximum {
        Err(HostError::CapacityExceeded { resource, maximum })
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};

    #[test]
    fn rejected_duplicate_define_preserves_the_accepted_value() {
        let mut builder = HostBuilder::new(HostPaths::new("/config", "/state", "/cache").unwrap());
        builder.define("answer", json!(42)).unwrap();
        assert!(matches!(
            builder.define("answer", Value::Null),
            Err(HostError::DuplicateDefine { .. })
        ));
        assert_eq!(builder.defines.get("answer"), Some(&json!(42)));
    }
}
