use crate::host::{FrozenCatalog, PROFILE_PLUGIN_ID};
use crate::{
    Host, HostError, HostPaths, ProfileControlContract, ProfileFragment, ProfileLimits,
    ProfilePatch, Result,
};
use rsi_meta::{
    ActivationPlan, ConfigValue, FactoryIdentity, LocalContract, LocalContractKey, LocalEvent,
    LocalEventKey, PluginFactory, PluginId, PreparedActivation, ResolvedFactory, Runtime,
    RuntimeLimits, UpdateMode,
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
    /// Maximum resolved factory registrations.
    pub maximum_factories: usize,
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
            maximum_factories: 4_096,
            maximum_fragments: 256,
            maximum_local_contracts: 4_096,
            maximum_local_events: 4_096,
            maximum_identifier_bytes: 256,
        }
    }
}

/// Freezes all generic Host composition inputs before Runtime creation.
pub struct HostBuilder {
    paths: Option<HostPaths>,
    platform: String,
    defines: BTreeMap<String, ConfigValue>,
    limits: HostLimits,
    runtime_limits: RuntimeLimits,
    execution: Option<rsi_meta::Execution>,
    factories: BTreeMap<PluginId, ResolvedFactory>,
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
            .field("factories", &self.factories.keys())
            .field("fragments", &self.fragment_ids)
            .finish_non_exhaustive()
    }
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
            .expect("factory remains available until destruction")
            .prepare(desired)
    }

    async fn activate(&self, plan: ActivationPlan) -> rsi_meta::Result<()> {
        self.inner
            .as_ref()
            .expect("factory remains available until destruction")
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
        Self::from_environment(
            Some(paths),
            format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH),
        )
    }

    /// Creates a builder for an embedder with no filesystem authority.
    pub fn without_paths(platform: impl Into<String>) -> Self {
        Self::from_environment(None, platform.into())
    }

    fn from_environment(paths: Option<HostPaths>, platform: String) -> Self {
        Self {
            paths,
            platform,
            defines: BTreeMap::new(),
            limits: HostLimits::default(),
            runtime_limits: RuntimeLimits::default(),
            execution: None,
            factories: BTreeMap::new(),
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

    /// Freezes explicit platform execution for startup without running any work.
    #[must_use]
    pub fn execution(mut self, execution: rsi_meta::Execution) -> Self {
        self.execution = Some(execution);
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
    pub const fn paths(&self) -> Option<&HostPaths> {
        self.paths.as_ref()
    }

    /// Registers one process-linked implementation without executing it.
    pub fn register_linked(
        &mut self,
        plugin: impl Into<PluginId>,
        revision: impl Into<String>,
        update_mode: UpdateMode,
        implementation: Arc<dyn PluginFactory>,
    ) -> Result<&mut Self> {
        self.register_factory(ResolvedFactory::linked(
            plugin,
            revision,
            update_mode,
            implementation,
        ))
    }

    /// Registers explicit trusted resolver provenance without executing the factory.
    /// Native identities come from the embedder's `NativeCatalog`; Host does not load artifacts.
    pub fn register_factory(&mut self, factory: ResolvedFactory) -> Result<&mut Self> {
        let (identity, update_mode, implementation) = factory.into_parts();
        // Contain destruction even when validation rejects the incoming registration.
        let factory = ResolvedFactory::new(
            identity,
            update_mode,
            Arc::new(ContainedFactory {
                inner: Some(implementation),
            }),
        );
        validate_limits(&self.limits)?;
        let plugin = validate_identity(factory.identity(), self.limits.maximum_identifier_bytes)?;
        if plugin.as_str() == PROFILE_PLUGIN_ID || self.factories.contains_key(plugin) {
            return Err(HostError::DuplicatePlugin {
                plugin: plugin.clone(),
            });
        }
        if self.factories.len() >= self.limits.maximum_factories {
            return Err(HostError::CapacityExceeded {
                resource: "factories",
                maximum: self.limits.maximum_factories,
            });
        }
        self.factories.insert(plugin.clone(), factory);
        Ok(self)
    }

    /// Reports whether this exact Local contract marker has already been registered.
    pub fn has_local_contract<C: LocalContract>(&self) -> bool {
        self.local_contract_keys.get(&LocalContractKey::new(C::KEY)) == Some(&TypeId::of::<C>())
    }

    /// Reports whether this exact Local event marker has already been registered.
    pub fn has_local_event<E: LocalEvent>(&self) -> bool {
        self.local_event_keys.get(&LocalEventKey::new(E::KEY)) == Some(&TypeId::of::<E>())
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
            "factories",
            self.factories.len(),
            self.limits.maximum_factories,
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
        for factory in self.factories.values() {
            validate_identity(factory.identity(), self.limits.maximum_identifier_bytes)?;
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
        let environment = match &self.paths {
            Some(paths) => ProfileEnvironment::new(
                paths.config(),
                paths.state(),
                paths.cache(),
                self.platform,
                self.defines,
            )?,
            None => ProfileEnvironment::without_paths(self.platform, self.defines)?,
        };
        environment.validate(&self.limits.profile)?;
        let runtime_limits = self.runtime_limits;
        Runtime::validate_limits(&runtime_limits)?;
        Ok(Host::new(
            self.paths,
            environment,
            self.limits,
            runtime_limits,
            self.execution,
            FrozenCatalog {
                factories: self.factories,
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

fn validate_identity(identity: &FactoryIdentity, maximum: usize) -> Result<&PluginId> {
    let plugin = match identity {
        FactoryIdentity::Linked { plugin, revision } => {
            validate_identifier("revision", revision, maximum)?;
            plugin
        }
        FactoryIdentity::Native { plugin, sha256 } => {
            if sha256.len() != 64
                || !sha256
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
            {
                return Err(HostError::InvalidNativeDigest);
            }
            plugin
        }
    };
    validate_identifier("plugin", plugin.as_str(), maximum)?;
    Ok(plugin)
}

fn validate_limits(limits: &HostLimits) -> Result<()> {
    for (resource, value) in [
        ("factories", limits.maximum_factories),
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
