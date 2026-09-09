use rsi_agent_composition::AgentContributionCatalog;
use rsi_host::{HostBuilder, HostError, HostLimits, ProfileFragment};
use rsi_meta::{
    ActivationPlan, LocalContract, LocalEvent, PluginFactory, ResolvedFactory, UpdateMode,
};
use serde::Serialize;
use serde_json::Value;
use sha2::{Digest as _, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

/// Maximum descriptive JSON bytes retained by one factory.
pub const MAXIMUM_FACTORY_DESCRIPTION_BYTES: usize = 65_536;
/// Maximum addon declarations in a frozen product assembly.
pub const MAXIMUM_STANDARD_ADDONS: usize = 128;
/// Maximum total serialized factory description bytes in one frozen assembly.
pub const MAXIMUM_ADDON_DESCRIPTION_BYTES: usize = 4 * 1024 * 1024;
/// Maximum nesting in descriptive configuration schemas.
pub const MAXIMUM_ADDON_SCHEMA_DEPTH: usize = 64;
/// Maximum explicit platform names in an addon declaration.
pub const MAXIMUM_ADDON_PLATFORMS: usize = 32;

/// Explicit placement of an ordinary addon factory or Profile fragment.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AddonScope {
    /// Independent service providers and explicitly enabled domain endpoints.
    Service,
    /// Contributions admitted only into a sealed Agent generation.
    Agent,
    /// Application behavior and build-time presentation assets.
    Application,
    /// Remote domain adapters; transport and endpoint authority remain independent.
    Client,
}

/// Public authoring information; never an activation validator.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct AddonFactoryDescription {
    /// Owning addon identity.
    pub addon: String,
    /// Exact linked factory identity.
    pub plugin: String,
    /// Selected composition role.
    pub scope: AddonScope,
    /// Explicit linked build revision.
    pub revision: String,
    /// Meta update behavior.
    pub update_mode: String,
    /// Bounded human-readable description.
    pub summary: String,
    /// Optional descriptive configuration schema. Prepare remains authoritative.
    pub configuration_schema: Option<Value>,
}

#[derive(Clone, Debug)]
struct Factory {
    description: AddonFactoryDescription,
    mode: UpdateMode,
    implementation: Arc<dyn PluginFactory>,
}

type RegisterMarker = fn(&mut HostBuilder) -> rsi_host::Result<()>;
type RegisterAgentMarker = fn(&mut AgentContributionCatalog) -> rsi_meta_profile::Result<()>;

#[derive(Clone, Debug)]
struct Marker {
    scope: AddonScope,
    lane: &'static str,
    key: &'static str,
    register: RegisterMarker,
    register_agent: RegisterAgentMarker,
}

#[derive(Clone, Debug)]
struct DomainExport {
    key: &'static str,
    publish: for<'a> fn(&mut ActivationPlan, DomainLookup<'a>) -> rsi_meta::Result<()>,
}

#[derive(Clone, Copy)]
pub(crate) enum DomainLookup<'a> {
    Local(&'a crate::ServiceHostConnection),
    Remote(&'a crate::ProfileOwner),
    #[cfg(target_os = "linux")]
    Service(&'a crate::RunningRsi),
}
impl DomainLookup<'_> {
    fn lookup<C: LocalContract>(self) -> Option<Arc<C::Service>> {
        match self {
            Self::Local(connection) => connection.lookup_addon::<C>(),
            Self::Remote(profile) => profile.lookup_local::<C>(),
            #[cfg(target_os = "linux")]
            Self::Service(service) => service.lookup_addon::<C>(),
        }
    }
}

/// Explicit mutable declaration; building executes no factory code.
#[derive(Debug)]
pub struct StandardAddonBuilder {
    addon: StandardAddon,
}

/// Immutable linked addon declaration. A Profile separately chooses activation.
#[derive(Clone, Debug)]
pub struct StandardAddon {
    id: String,
    platforms: BTreeSet<String>,
    factories: BTreeMap<String, Factory>,
    markers: Vec<Marker>,
    fragments: Vec<(AddonScope, ProfileFragment)>,
    exports: Vec<DomainExport>,
}

impl StandardAddonBuilder {
    /// Starts a portable declaration with an explicit product identity.
    pub fn new(id: impl Into<String>) -> Self {
        Self {
            addon: StandardAddon {
                id: id.into(),
                platforms: BTreeSet::new(),
                factories: BTreeMap::new(),
                markers: Vec::new(),
                fragments: Vec::new(),
                exports: Vec::new(),
            },
        }
    }

    /// Restricts this declaration to exact Host platform strings (for example linux-x86_64).
    /// An empty set is portable; an unsupported platform fails instead of silently omitting code.
    pub fn platforms(
        &mut self,
        platforms: impl IntoIterator<Item = String>,
    ) -> rsi_host::Result<&mut Self> {
        let mut selected = BTreeSet::new();
        for platform in platforms {
            if selected.len() >= MAXIMUM_ADDON_PLATFORMS {
                return Err(capacity("addon platforms", MAXIMUM_ADDON_PLATFORMS));
            }
            identifier("addon platform", &platform)?;
            selected.insert(platform);
        }
        self.addon.platforms = selected;
        Ok(self)
    }

    /// Declares a linked Service factory without enabling it.
    pub fn register_linked(
        &mut self,
        plugin: impl Into<String>,
        revision: impl Into<String>,
        mode: UpdateMode,
        implementation: Arc<dyn PluginFactory>,
    ) -> rsi_host::Result<&mut Self> {
        self.register_factory(AddonScope::Service, plugin, revision, mode, implementation)
    }

    /// Declares one factory in its explicit composition role.
    pub fn register_factory(
        &mut self,
        scope: AddonScope,
        plugin: impl Into<String>,
        revision: impl Into<String>,
        mode: UpdateMode,
        implementation: Arc<dyn PluginFactory>,
    ) -> rsi_host::Result<&mut Self> {
        let plugin = plugin.into();
        if self.addon.factories.contains_key(&plugin) {
            return Err(HostError::DuplicatePlugin {
                plugin: plugin.into(),
            });
        }
        if self.addon.factories.len() >= HostLimits::default().maximum_factories {
            return Err(capacity(
                "addon factories",
                HostLimits::default().maximum_factories,
            ));
        }
        let revision = revision.into();
        identifier("addon factory", &plugin)?;
        identifier("addon revision", &revision)?;
        self.addon.factories.insert(
            plugin.clone(),
            Factory {
                description: AddonFactoryDescription {
                    addon: self.addon.id.clone(),
                    plugin,
                    scope,
                    revision,
                    update_mode: match mode {
                        UpdateMode::Replayable => "replayable",
                        UpdateMode::RestartRequired => "restart_required",
                    }
                    .into(),
                    summary: String::new(),
                    configuration_schema: None,
                },
                mode,
                implementation,
            },
        );
        Ok(self)
    }

    /// Attaches bounded descriptive metadata to an already declared factory.
    pub fn describe_factory(
        &mut self,
        plugin: &str,
        summary: impl Into<String>,
        configuration_schema: Option<Value>,
    ) -> rsi_host::Result<&mut Self> {
        if let Some(schema) = &configuration_schema {
            validate_schema_depth(schema)?;
        }
        let factory = self.addon.factories.get_mut(plugin).ok_or_else(|| {
            HostError::Bootstrap(format!("cannot describe undeclared factory `{plugin}`"))
        })?;
        let mut description = factory.description.clone();
        description.summary = summary.into();
        description.configuration_schema = configuration_schema;
        let _ = bounded_json(&description)?;
        factory.description = description;
        Ok(self)
    }

    /// Declares an exact Service Local marker. Exact repeats are shared during assembly.
    pub fn register_local_contract<C: LocalContract>(&mut self) -> rsi_host::Result<&mut Self> {
        self.register_local_contract_at::<C>(AddonScope::Service)
    }

    /// Declares an exact Local marker in a selected composition role.
    pub fn register_local_contract_at<C: LocalContract>(
        &mut self,
        scope: AddonScope,
    ) -> rsi_host::Result<&mut Self> {
        self.marker(
            scope,
            "local",
            C::KEY,
            |builder| {
                if !builder.has_local_contract::<C>() {
                    builder.register_local_contract::<C>()?;
                }
                Ok(())
            },
            AgentContributionCatalog::register_local_contract::<C>,
        )
    }

    /// Declares an exact event marker in a selected composition role.
    pub fn register_local_event_at<E: LocalEvent>(
        &mut self,
        scope: AddonScope,
    ) -> rsi_host::Result<&mut Self> {
        self.marker(
            scope,
            "event",
            E::KEY,
            |builder| {
                if !builder.has_local_event::<E>() {
                    builder.register_local_event::<E>()?;
                }
                Ok(())
            },
            AgentContributionCatalog::register_local_event::<E>,
        )
    }

    fn marker(
        &mut self,
        scope: AddonScope,
        lane: &'static str,
        key: &'static str,
        register: RegisterMarker,
        register_agent: RegisterAgentMarker,
    ) -> rsi_host::Result<&mut Self> {
        if self.addon.markers.len() >= HostLimits::default().maximum_local_contracts {
            return Err(capacity(
                "addon markers",
                HostLimits::default().maximum_local_contracts,
            ));
        }
        self.addon.markers.push(Marker {
            scope,
            lane,
            key,
            register,
            register_agent,
        });
        Ok(self)
    }

    /// Explicitly forwards one domain capability into its application from the selected
    /// embedded service or remote client. Both domain implementations remain addon-owned.
    pub fn export_domain<C: LocalContract>(&mut self) -> rsi_host::Result<&mut Self> {
        if self.addon.exports.iter().any(|export| export.key == C::KEY) {
            return Err(HostError::Bootstrap(format!(
                "duplicate exported domain `{}`",
                C::KEY
            )));
        }
        for scope in [
            AddonScope::Service,
            AddonScope::Client,
            AddonScope::Application,
        ] {
            self.register_local_contract_at::<C>(scope)?;
        }
        self.addon.exports.push(DomainExport {
            key: C::KEY,
            publish: |plan, source| {
                let service = source.lookup::<C>().ok_or_else(|| {
                    rsi_meta::MetaError::Activation(format!(
                        "declared addon domain `{}` is unavailable",
                        C::KEY
                    ))
                })?;
                let supply = plan.context().provide_local::<C>(service)?;
                plan.defer(
                    "withdraw addon domain",
                    Box::new(move || {
                        Box::pin(async move {
                            drop(supply);
                            Ok(())
                        })
                    }),
                )
            },
        });
        Ok(self)
    }

    /// Declares an explicit Service Profile fragment, including any selected activations.
    pub fn register_fragment(&mut self, fragment: ProfileFragment) -> rsi_host::Result<&mut Self> {
        self.register_fragment_at(AddonScope::Service, fragment)
    }

    /// Declares an explicit fragment for a selected product role.
    pub fn register_fragment_at(
        &mut self,
        scope: AddonScope,
        fragment: ProfileFragment,
    ) -> rsi_host::Result<&mut Self> {
        if scope == AddonScope::Agent {
            return Err(HostError::Bootstrap(
                "Agent activation is selected by its preset Profile".into(),
            ));
        }
        if self.addon.fragments.len() >= HostLimits::default().maximum_fragments {
            return Err(capacity(
                "addon fragments",
                HostLimits::default().maximum_fragments,
            ));
        }
        self.addon.fragments.push((scope, fragment));
        Ok(self)
    }

    /// Freezes and structurally checks the declaration without prepare or activation.
    pub fn build(self) -> rsi_host::Result<StandardAddon> {
        identifier("addon", &self.addon.id)?;
        for platform in &self.addon.platforms {
            identifier("addon platform", platform)?;
        }
        StandardAddonSet::new([self.addon.clone()])?;
        Ok(self.addon)
    }
}

/// Frozen declarations shared by product preview, compiler, and startup paths.
#[derive(Clone, Debug, Default)]
pub struct StandardAddonSet {
    addons: Arc<[StandardAddon]>,
}

impl StandardAddonSet {
    /// Freezes an ordered set, rejecting all duplicate identities before use.
    pub fn new(addons: impl IntoIterator<Item = StandardAddon>) -> rsi_host::Result<Self> {
        let mut frozen = Vec::new();
        let mut ids = BTreeSet::new();
        let mut plugins = BTreeSet::new();
        let mut exports = BTreeSet::new();
        let mut description_bytes = 0_usize;
        for addon in addons {
            if frozen.len() >= MAXIMUM_STANDARD_ADDONS {
                return Err(capacity("standard addons", MAXIMUM_STANDARD_ADDONS));
            }
            if !ids.insert(addon.id.clone()) {
                return Err(HostError::Bootstrap(format!(
                    "duplicate addon `{}`",
                    addon.id
                )));
            }
            for plugin in addon.factories.keys() {
                if plugins.len() >= HostLimits::default().maximum_factories {
                    return Err(capacity(
                        "addon factories",
                        HostLimits::default().maximum_factories,
                    ));
                }
                if !plugins.insert(plugin.clone()) {
                    return Err(HostError::DuplicatePlugin {
                        plugin: plugin.clone().into(),
                    });
                }
            }
            for factory in addon.factories.values() {
                description_bytes += bounded_json(&factory.description)?.len();
                if description_bytes > MAXIMUM_ADDON_DESCRIPTION_BYTES {
                    return Err(capacity(
                        "addon descriptions",
                        MAXIMUM_ADDON_DESCRIPTION_BYTES,
                    ));
                }
            }
            for export in &addon.exports {
                if !exports.insert(export.key) {
                    return Err(HostError::Bootstrap(format!(
                        "duplicate exported domain `{}`",
                        export.key
                    )));
                }
            }
            frozen.push(addon);
        }
        let set = Self {
            addons: frozen.into(),
        };
        for scope in [
            AddonScope::Service,
            AddonScope::Agent,
            AddonScope::Application,
            AddonScope::Client,
        ] {
            let mut builder = HostBuilder::without_paths("addon-validation");
            set.register_into(&mut builder, scope)?;
            builder.build()?;
        }
        Ok(set)
    }

    /// Enumerates bounded authoring descriptions without invoking factories.
    pub fn descriptions(&self) -> impl Iterator<Item = &AddonFactoryDescription> {
        self.addons
            .iter()
            .flat_map(|addon| addon.factories.values().map(|factory| &factory.description))
    }

    /// Registers one role into a still-mutable generic Host catalog.
    pub fn register_into(
        &self,
        builder: &mut HostBuilder,
        scope: AddonScope,
    ) -> rsi_host::Result<()> {
        for addon in self.addons.iter() {
            for marker in addon.markers.iter().filter(|marker| marker.scope == scope) {
                (marker.register)(builder)?;
            }
            for factory in addon
                .factories
                .values()
                .filter(|factory| factory.description.scope == scope)
            {
                builder.register_linked(
                    factory.description.plugin.as_str(),
                    factory.description.revision.clone(),
                    factory.mode,
                    factory.implementation.clone(),
                )?;
            }
            for (_, fragment) in addon.fragments.iter().filter(|(role, _)| *role == scope) {
                builder.register_fragment(fragment.clone())?;
            }
        }
        Ok(())
    }

    pub(crate) fn publish_domains(
        &self,
        plan: &mut ActivationPlan,
        source: DomainLookup<'_>,
    ) -> rsi_meta::Result<()> {
        for addon in self.addons.iter() {
            for export in &addon.exports {
                (export.publish)(plan, source)?;
            }
        }
        Ok(())
    }

    pub(crate) fn merged(&self, addon: StandardAddon) -> rsi_host::Result<Self> {
        Self::new(std::iter::once(addon).chain(self.addons.iter().cloned()))
    }

    pub(crate) fn validate_platform(&self, platform: &str) -> rsi_host::Result<()> {
        for addon in self.addons.iter() {
            if !addon.platforms.is_empty() && !addon.platforms.contains(platform) {
                return Err(HostError::Bootstrap(format!(
                    "addon `{}` does not support `{platform}`",
                    addon.id
                )));
            }
        }
        Ok(())
    }

    pub(crate) fn agent_catalog(&self) -> rsi_host::Result<AgentContributionCatalog> {
        let mut catalog = AgentContributionCatalog::new(
            self.addons
                .iter()
                .flat_map(|addon| addon.factories.values())
                .filter(|factory| factory.description.scope == AddonScope::Agent)
                .map(|factory| {
                    ResolvedFactory::linked(
                        factory.description.plugin.as_str(),
                        factory.description.revision.clone(),
                        factory.mode,
                        factory.implementation.clone(),
                    )
                }),
        )
        .map_err(|error| HostError::Bootstrap(error.to_string()))?;
        for marker in self
            .addons
            .iter()
            .flat_map(|addon| &addon.markers)
            .filter(|marker| marker.scope == AddonScope::Agent)
        {
            (marker.register_agent)(&mut catalog)
                .map_err(|error| HostError::Bootstrap(error.to_string()))?;
        }
        Ok(catalog)
    }

    pub(crate) fn digest(&self) -> rsi_host::Result<String> {
        let mut digest = Sha256::new();
        digest.update(b"rsi.standard.addons.v1");
        for addon in self.addons.iter() {
            component(&mut digest, b"addon-id");
            component(&mut digest, addon.id.as_bytes());
            component(&mut digest, b"platforms");
            component(&mut digest, &(addon.platforms.len() as u64).to_le_bytes());
            for platform in &addon.platforms {
                component(&mut digest, platform.as_bytes());
            }
            for factory in addon.factories.values() {
                component(&mut digest, b"factory");
                component(&mut digest, &bounded_json(&factory.description)?);
            }
            for marker in &addon.markers {
                component(&mut digest, b"marker");
                component(&mut digest, &bounded_json(&marker.scope)?);
                component(&mut digest, marker.lane.as_bytes());
                component(&mut digest, marker.key.as_bytes());
            }
            for (scope, fragment) in &addon.fragments {
                component(&mut digest, b"fragment");
                component(&mut digest, &bounded_json(scope)?);
                component(&mut digest, fragment.source_digest().as_bytes());
            }
            for export in &addon.exports {
                component(&mut digest, b"export-domain");
                component(&mut digest, export.key.as_bytes());
            }
            component(&mut digest, b"addon-end");
        }
        Ok(hex::encode(digest.finalize()))
    }
}

fn component(digest: &mut Sha256, bytes: &[u8]) {
    digest.update((bytes.len() as u64).to_le_bytes());
    digest.update(bytes);
}

fn identifier(kind: &'static str, value: &str) -> rsi_host::Result<()> {
    let maximum = HostLimits::default().maximum_identifier_bytes;
    if value.is_empty() || value.len() > maximum {
        return Err(HostError::InvalidIdentifier { kind, maximum });
    }
    Ok(())
}

fn capacity(resource: &'static str, maximum: usize) -> HostError {
    HostError::CapacityExceeded { resource, maximum }
}

fn validate_schema_depth(schema: &Value) -> rsi_host::Result<()> {
    let mut pending: Vec<Box<dyn Iterator<Item = &Value>>> =
        vec![Box::new(std::iter::once(schema))];
    let mut nodes = 0_usize;
    while let Some(children) = pending.last_mut() {
        let Some(value) = children.next() else {
            pending.pop();
            continue;
        };
        if pending.len() > MAXIMUM_ADDON_SCHEMA_DEPTH + 1 {
            return Err(capacity("addon schema depth", MAXIMUM_ADDON_SCHEMA_DEPTH));
        }
        nodes += 1;
        if nodes > MAXIMUM_FACTORY_DESCRIPTION_BYTES {
            return Err(capacity(
                "addon schema nodes",
                MAXIMUM_FACTORY_DESCRIPTION_BYTES,
            ));
        }
        match value {
            Value::Array(values) => pending.push(Box::new(values.iter())),
            Value::Object(values) => pending.push(Box::new(values.values())),
            _ => {}
        }
    }
    Ok(())
}

fn bounded_json(value: &impl Serialize) -> rsi_host::Result<Vec<u8>> {
    struct Bounded(Vec<u8>);
    impl std::io::Write for Bounded {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            if bytes.len() > MAXIMUM_FACTORY_DESCRIPTION_BYTES.saturating_sub(self.0.len()) {
                return Err(std::io::Error::other(
                    "factory description exceeds its byte limit",
                ));
            }
            self.0.write_all(bytes)?;
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut output = Bounded(Vec::new());
    serde_json::to_writer(&mut output, value)
        .map_err(|error| HostError::Bootstrap(error.to_string()))?;
    Ok(output.0)
}
