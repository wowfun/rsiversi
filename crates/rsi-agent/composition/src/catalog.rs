use crate::{MAXIMUM_CATALOG_FACTORIES, MAXIMUM_CATALOG_KEY_BYTES, MAXIMUM_CATALOG_MARKERS};
use rsi_meta::{FactoryIdentity, PluginId, ResolvedFactory};
use rsi_meta_profile::{ProfileError, ProfileResolver};
use sha2::{Digest, Sha256};
use std::any::TypeId;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::hash::{DefaultHasher, Hash, Hasher};

/// Frozen Agent-only allowlist of exact resolved contribution factories.
#[derive(Clone)]
pub struct AgentContributionCatalog {
    factories: BTreeMap<PluginId, ResolvedFactory>,
    local_contracts: BTreeMap<String, TypeId>,
    local_events: BTreeMap<String, TypeId>,
    portable_isolations: BTreeSet<String>,
}

impl AgentContributionCatalog {
    /// Freezes exact executable identities selected by the application.
    ///
    /// Duplicate plugin identities are rejected rather than resolved by input
    /// order.
    ///
    /// # Errors
    ///
    /// Returns [`ProfileError::InvalidProgram`] when the input repeats one
    /// plugin identity, or [`ProfileError::CapacityExceeded`] above the factory bound.
    pub fn new(
        factories: impl IntoIterator<Item = ResolvedFactory>,
    ) -> rsi_meta_profile::Result<Self> {
        let mut by_plugin = BTreeMap::new();
        for factory in factories {
            if by_plugin.len() >= MAXIMUM_CATALOG_FACTORIES {
                return Err(ProfileError::CapacityExceeded {
                    resource: "Agent catalog factories",
                    maximum: MAXIMUM_CATALOG_FACTORIES,
                });
            }
            let plugin = match factory.identity() {
                FactoryIdentity::Linked { plugin, .. } | FactoryIdentity::Native { plugin, .. } => {
                    plugin.clone()
                }
            };
            if by_plugin.insert(plugin.clone(), factory).is_some() {
                return Err(ProfileError::InvalidProgram(format!(
                    "Agent contribution factory `{plugin}` appears more than once"
                )));
            }
        }
        Ok(Self {
            factories: by_plugin,
            local_contracts: BTreeMap::new(),
            local_events: BTreeMap::new(),
            portable_isolations: BTreeSet::new(),
        })
    }

    /// Selects a Local marker before transferring this catalog into a factory.
    ///
    /// # Errors
    /// Rejects another nominal marker at the same key or the per-lane capacity limit.
    pub fn register_local_contract<C: rsi_meta::LocalContract>(
        &mut self,
    ) -> rsi_meta_profile::Result<()> {
        register_marker(&mut self.local_contracts, C::KEY, TypeId::of::<C>())
    }

    /// Selects an event marker before transferring this catalog into a factory.
    ///
    /// # Errors
    /// Rejects another nominal marker at the same key or the per-lane capacity limit.
    pub fn register_local_event<E: rsi_meta::LocalEvent>(
        &mut self,
    ) -> rsi_meta_profile::Result<()> {
        register_marker(&mut self.local_events, E::KEY, TypeId::of::<E>())
    }

    /// Declares a Portable key shared privately by leaves in each generation.
    ///
    /// # Errors
    /// Rejects empty/oversized keys and a full declaration lane. Repeating a key
    /// is idempotent. The Runtime may impose a stricter Context budget.
    pub fn isolate_portable(&mut self, key: impl Into<String>) -> rsi_meta_profile::Result<()> {
        let key = key.into();
        if key.is_empty() || key.len() > MAXIMUM_CATALOG_KEY_BYTES {
            return Err(ProfileError::InvalidProgram(
                "invalid Agent Portable isolation key".into(),
            ));
        }
        if !self.portable_isolations.contains(&key)
            && self.portable_isolations.len() >= MAXIMUM_CATALOG_MARKERS
        {
            return Err(ProfileError::CapacityExceeded {
                resource: "Agent Portable isolation keys",
                maximum: MAXIMUM_CATALOG_MARKERS,
            });
        }
        self.portable_isolations.insert(key);
        Ok(())
    }

    pub(crate) fn isolate(
        &self,
        mut context: rsi_meta::Context,
    ) -> rsi_meta::Result<rsi_meta::Context> {
        for key in &self.portable_isolations {
            (context, _) = context.isolate_fresh(key)?;
        }
        Ok(context)
    }

    pub(crate) fn same_identity(&self, other: &Self) -> bool {
        self.factories.len() == other.factories.len()
            && self
                .factories
                .iter()
                .zip(&other.factories)
                .all(|((a_key, a), (b_key, b))| {
                    a_key == b_key
                        && a.identity() == b.identity()
                        && a.update_mode() == b.update_mode()
                })
            && self.local_contracts == other.local_contracts
            && self.local_events == other.local_events
            && self.portable_isolations == other.portable_isolations
    }

    pub(crate) fn digest(&self) -> String {
        let mut digest = Sha256::new();
        digest.update(b"rsi.agent.catalog.v1");
        digest.update((self.factories.len() as u64).to_le_bytes());
        for factory in self.factories.values() {
            hash_field(
                &mut digest,
                &serde_json::to_vec(&(factory.identity(), factory.update_mode()))
                    .expect("factory identity serializes"),
            );
        }
        for lane in [&self.local_contracts, &self.local_events] {
            digest.update((lane.len() as u64).to_le_bytes());
            for (key, nominal) in lane {
                hash_field(&mut digest, key.as_bytes());
                // TypeId has process-local equality, not a durable serialization.
                // The cache additionally compares the complete typed signature.
                let mut hasher = DefaultHasher::new();
                nominal.hash(&mut hasher);
                digest.update(hasher.finish().to_le_bytes());
            }
        }
        digest.update((self.portable_isolations.len() as u64).to_le_bytes());
        for key in &self.portable_isolations {
            hash_field(&mut digest, key.as_bytes());
        }
        format!("{:x}", digest.finalize())
    }
}

fn register_marker(
    markers: &mut BTreeMap<String, TypeId>,
    key: &str,
    nominal: TypeId,
) -> rsi_meta_profile::Result<()> {
    if let Some(existing) = markers.get(key) {
        return if *existing == nominal {
            Ok(())
        } else {
            Err(ProfileError::InvalidProgram(
                "conflicting Agent nominal marker".into(),
            ))
        };
    }
    if markers.len() >= MAXIMUM_CATALOG_MARKERS {
        return Err(ProfileError::CapacityExceeded {
            resource: "Agent catalog markers",
            maximum: MAXIMUM_CATALOG_MARKERS,
        });
    }
    markers.insert(key.to_owned(), nominal);
    Ok(())
}

impl fmt::Debug for AgentContributionCatalog {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AgentContributionCatalog")
            .field("factories", &self.factories.keys())
            .field("local_contracts", &self.local_contracts.keys())
            .field("local_events", &self.local_events.keys())
            .field("portable_isolations", &self.portable_isolations)
            .finish()
    }
}

impl ProfileResolver for AgentContributionCatalog {
    fn resolve(&self, plugin: &PluginId) -> rsi_meta_profile::Result<ResolvedFactory> {
        self.factories
            .get(plugin)
            .cloned()
            .ok_or_else(|| ProfileError::UnknownPlugin {
                plugin: plugin.clone(),
            })
    }

    fn local_contract_type(&self, key: &str) -> rsi_meta_profile::Result<TypeId> {
        self.local_contracts
            .get(key)
            .copied()
            .ok_or_else(|| ProfileError::UnknownLocalContract {
                key: key.to_owned(),
            })
    }

    fn local_event_type(&self, key: &str) -> rsi_meta_profile::Result<TypeId> {
        self.local_events
            .get(key)
            .copied()
            .ok_or_else(|| ProfileError::UnknownLocalEvent {
                key: key.to_owned(),
            })
    }
}

pub(crate) fn hash_field(digest: &mut Sha256, bytes: &[u8]) {
    digest.update((bytes.len() as u64).to_le_bytes());
    digest.update(bytes);
}
