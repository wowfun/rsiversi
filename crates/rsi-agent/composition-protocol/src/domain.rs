//! Typed definitions and exact frozen-generation proposals. No persistence authority.

use rsi_agent_session_protocol::{
    DomainIdentity, DomainMutationSource, DomainRevision, DomainSnapshot, DomainStateCommit,
    DomainStateUpdate, DomainStateValue, MAXIMUM_DOMAIN_BASELINE_BYTES, MAXIMUM_SESSION_DOMAINS,
};
use serde::{Serialize, de::DeserializeOwned};
use std::{collections::BTreeMap, fmt, marker::PhantomData, sync::Arc};
use thiserror::Error;

/// Domain contract failures before any mutation is admitted.
#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum DomainError {
    /// State encoding or semantic validation failed.
    #[error("invalid domain {domain} state: {reason}")]
    InvalidState {
        /// Owning domain name.
        domain: String,
        /// Bounded owner-supplied diagnostic.
        reason: String,
    },
    /// The exact state version is unavailable.
    #[error("unsupported domain codec: {0:?}")]
    Unsupported(DomainIdentity),
    /// Execution requires a frozen state for every declared domain.
    #[error("domain baseline is missing required state: {0:?}")]
    MissingState(DomainIdentity),
    /// Draft replacement is initial state, not a mutation of a durable revision.
    #[error("draft baseline proposals require expected revision zero")]
    BaselineRevision,
    /// A definition or proposal came from another frozen generation.
    #[error("domain handle belongs to another generation")]
    WrongGeneration,
    /// Two definitions claim one durable name.
    #[error("duplicate domain definition: {0}")]
    Duplicate(String),
    /// Domain count or aggregate initial-state bytes exceed their bound.
    #[error("domain catalog capacity exceeded")]
    Capacity,
    /// The unpublished stage no longer admits definitions.
    #[error("domain registration stage is closed")]
    Closed,
    /// The exact Runtime generation cannot publish this registration.
    #[error("domain registration ownership is unavailable")]
    RegistrationUnavailable,
}

type Result<T> = std::result::Result<T, DomainError>;

trait Codec: fmt::Debug + Send + Sync {
    fn identity(&self) -> &DomainIdentity;
    fn initial(&self) -> &DomainSnapshot;
    fn validate(&self, state: &DomainStateValue) -> Result<()>;
}

struct TypedCodec<T> {
    initial: DomainSnapshot,
    validate: fn(&T) -> std::result::Result<(), String>,
    marker: PhantomData<T>,
}

impl<T> fmt::Debug for TypedCodec<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DomainDefinition")
            .field("identity", self.initial.identity())
            .finish_non_exhaustive()
    }
}

impl<T: DeserializeOwned + Send + Sync> TypedCodec<T> {
    fn decode(&self, state: &DomainStateValue) -> Result<T> {
        let value = T::deserialize(state.value()).map_err(|_| {
            invalid(
                self.initial.identity(),
                "state does not match its typed codec",
            )
        })?;
        (self.validate)(&value).map_err(|reason| invalid(self.initial.identity(), &reason))?;
        Ok(value)
    }
}

impl<T: DeserializeOwned + Send + Sync> Codec for TypedCodec<T> {
    fn identity(&self) -> &DomainIdentity {
        self.initial.identity()
    }
    fn initial(&self) -> &DomainSnapshot {
        &self.initial
    }
    fn validate(&self, state: &DomainStateValue) -> Result<()> {
        self.decode(state).map(|_| ())
    }
}

/// Typed state authority: exact codec identity, validated initial state and pure validator.
pub struct DomainDefinition<T> {
    codec: Arc<TypedCodec<T>>,
}

impl<T> Clone for DomainDefinition<T> {
    fn clone(&self) -> Self {
        Self {
            codec: self.codec.clone(),
        }
    }
}

impl<T> fmt::Debug for DomainDefinition<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.codec.fmt(f)
    }
}

impl<T: Serialize + DeserializeOwned + Send + Sync + 'static> DomainDefinition<T> {
    /// Validates and freezes an initial value with its effect-free semantic validator.
    ///
    /// # Errors
    /// Rejects semantic, serialization, or structural failures as `InvalidState`.
    pub fn new(
        identity: DomainIdentity,
        initial: &T,
        validate: fn(&T) -> std::result::Result<(), String>,
    ) -> Result<Self> {
        validate(initial).map_err(|reason| invalid(&identity, &reason))?;
        let state = encode(&identity, initial)?;
        let codec = Arc::new(TypedCodec {
            initial: DomainSnapshot::new(identity, state),
            validate,
            marker: PhantomData,
        });
        // Validate the serialized representation as well: linked serializers may differ from T.
        codec.validate(codec.initial.state())?;
        Ok(Self { codec })
    }
    /// Returns the durable codec identity.
    pub fn identity(&self) -> &DomainIdentity {
        self.codec.identity()
    }
    /// Erases only the state type for a frozen heterogeneous catalog.
    pub fn registration(&self) -> DomainRegistration {
        DomainRegistration(self.codec.clone())
    }
    /// Registers under the caller's exact Meta generation and returns its typed handle and lease.
    ///
    /// # Errors
    /// Propagates closed-stage, duplicate, capacity, or lifecycle admission errors.
    pub fn register(
        &self,
        registrar: &dyn DomainRegistrar,
        context: &rsi_meta::RegistrationContext,
    ) -> Result<(DomainHandle<T>, rsi_meta::RegistrationLease)> {
        let (binding, lease) = registrar.register(context, self.registration())?;
        Ok((self.bind(&binding)?, lease))
    }
    /// Recovers a typed handle from this definition's exact staged registration.
    ///
    /// # Errors
    /// Returns `WrongGeneration` when the binding does not own this definition.
    pub fn bind(&self, binding: &DomainBinding) -> Result<DomainHandle<T>> {
        let codec: Arc<dyn Codec> = self.codec.clone();
        if !Arc::ptr_eq(&codec, &binding.entry.codec) {
            return Err(DomainError::WrongGeneration);
        }
        Ok(DomainHandle {
            definition: self.clone(),
            binding: binding.clone(),
        })
    }
}

/// Opaque validated definition accepted by a generation's composition stage.
#[derive(Clone, Debug)]
pub struct DomainRegistration(Arc<dyn Codec>);

/// Opaque exact staged registration; withdrawal cannot revoke a later replacement.
#[derive(Clone, Debug)]
pub struct DomainBinding {
    generation: Arc<()>,
    entry: Arc<Entry>,
}

impl DomainBinding {
    /// Returns the definition identity; this is not mutation authority.
    pub fn identity(&self) -> &DomainIdentity {
        self.entry.codec.identity()
    }
}

/// Write-only definition admission for one unpublished Agent generation.
pub trait DomainRegistrar: fmt::Debug + Send + Sync + 'static {
    /// Owns registration with an exact lifecycle credential and withdrawable lease.
    ///
    /// # Errors
    /// Rejects closed stages, duplicate names, exhausted capacity or unavailable owners.
    fn register(
        &self,
        context: &rsi_meta::RegistrationContext,
        definition: DomainRegistration,
    ) -> Result<(DomainBinding, rsi_meta::RegistrationLease)>;
}

/// Nominal Local contract for the unpublished domain registrar.
#[derive(Debug)]
pub struct DomainRegistrarContract;
impl rsi_meta_contract::LocalContract for DomainRegistrarContract {
    const KEY: &'static str = "rsi.agent.domain-registrar";
    type Service = dyn DomainRegistrar;
}

#[derive(Debug)]
struct Entry {
    codec: Arc<dyn Codec>,
}

/// Pure unpublished definition set. Its host owns registration lifecycle and sealing.
#[derive(Debug, Default)]
pub struct DomainCatalogBuilder {
    generation: Arc<()>,
    definitions: BTreeMap<String, Arc<Entry>>,
}

impl DomainCatalogBuilder {
    /// Creates one unique empty generation.
    pub fn new() -> Self {
        Self::default()
    }
    /// Admits one unique definition and returns its exact registration identity.
    ///
    /// # Errors
    /// Returns `Duplicate` or `Capacity` before changing the set.
    pub fn register(&mut self, definition: DomainRegistration) -> Result<DomainBinding> {
        let id = definition.0.identity().id();
        if self.definitions.contains_key(id) {
            return Err(DomainError::Duplicate(id.into()));
        }
        if self.definitions.len() >= MAXIMUM_SESSION_DOMAINS {
            return Err(DomainError::Capacity);
        }
        let id = id.to_owned();
        let entry = Arc::new(Entry {
            codec: definition.0,
        });
        self.definitions.insert(id, entry.clone());
        Ok(DomainBinding {
            generation: self.generation.clone(),
            entry,
        })
    }
    /// Withdraws only the matching live registration in this unpublished set.
    pub fn withdraw(&mut self, binding: &DomainBinding) -> bool {
        let id = binding.entry.codec.identity().id();
        if Arc::ptr_eq(&self.generation, &binding.generation)
            && self
                .definitions
                .get(id)
                .is_some_and(|entry| Arc::ptr_eq(entry, &binding.entry))
        {
            self.definitions.remove(id);
            true
        } else {
            false
        }
    }
    /// Consumes the mutable set and freezes its aggregate-bounded initial states.
    ///
    /// # Errors
    /// Returns `Capacity` when the combined baseline exceeds its encoded byte limit.
    pub fn finish(self) -> Result<DomainCatalog> {
        let mut bytes = 0_usize;
        for entry in self.definitions.values() {
            let size = entry
                .codec
                .initial()
                .encoded_len()
                .map_err(|_| DomainError::Capacity)?;
            bytes = bytes.checked_add(size).ok_or(DomainError::Capacity)?;
            if bytes > MAXIMUM_DOMAIN_BASELINE_BYTES {
                return Err(DomainError::Capacity);
            }
        }
        let baseline = self
            .definitions
            .values()
            .map(|entry| entry.codec.initial().clone())
            .collect();
        Ok(DomainCatalog {
            inner: Arc::new(Catalog {
                generation: self.generation,
                definitions: self.definitions,
                baseline,
            }),
        })
    }
}

/// Unique definitions and their bounded baseline, immutable after construction.
#[derive(Clone, Debug)]
pub struct DomainCatalog {
    inner: Arc<Catalog>,
}

#[derive(Debug)]
struct Catalog {
    generation: Arc<()>,
    definitions: BTreeMap<String, Arc<Entry>>,
    baseline: Arc<[DomainSnapshot]>,
}

impl DomainCatalog {
    /// Freezes unique domain names and validates aggregate initial-state bounds.
    ///
    /// # Errors
    /// Rejects duplicate names, excessive definition count or oversized baselines.
    pub fn new(definitions: impl IntoIterator<Item = DomainRegistration>) -> Result<Self> {
        let mut stage = DomainCatalogBuilder::new();
        for definition in definitions {
            stage.register(definition)?;
        }
        stage.finish()
    }

    /// Binds a typed definition to this exact frozen catalog, without exposing a writer.
    ///
    /// # Errors
    /// Returns `WrongGeneration` unless the exact definition belongs to this catalog.
    pub fn bind<T: Serialize + DeserializeOwned + Send + Sync + 'static>(
        &self,
        definition: &DomainDefinition<T>,
    ) -> Result<DomainHandle<T>> {
        let entry = self
            .inner
            .definitions
            .get(definition.identity().id())
            .ok_or(DomainError::WrongGeneration)?;
        definition.bind(&DomainBinding {
            generation: self.inner.generation.clone(),
            entry: entry.clone(),
        })
    }

    /// Borrows complete initial states in stable domain-name order.
    pub fn baseline(&self) -> &[DomainSnapshot] {
        &self.inner.baseline
    }

    /// Checks codec availability and semantic state validity for execution admission.
    ///
    /// # Errors
    /// Returns `Unsupported` for missing versions or `InvalidState` for invalid payloads.
    pub fn validate_snapshot(&self, snapshot: &DomainSnapshot) -> Result<()> {
        let identity = snapshot.identity();
        let entry = self
            .inner
            .definitions
            .get(identity.id())
            .filter(|entry| entry.codec.identity() == identity)
            .ok_or_else(|| DomainError::Unsupported(identity.clone()))?;
        entry.codec.validate(snapshot.state())
    }

    /// Validates the entire frozen set without importing defaults for missing state.
    ///
    /// # Errors
    /// Rejects missing/unsupported codecs, duplicate names, oversized sets or invalid typed values.
    pub fn validate_complete_states(&self, states: &[DomainSnapshot]) -> Result<()> {
        if states.len() > MAXIMUM_SESSION_DOMAINS {
            return Err(DomainError::Capacity);
        }
        let mut seen = std::collections::BTreeSet::new();
        let mut bytes = 0_usize;
        for snapshot in states {
            if !seen.insert(snapshot.identity().id()) {
                return Err(DomainError::Duplicate(snapshot.identity().id().into()));
            }
            bytes = bytes
                .checked_add(snapshot.encoded_len().map_err(|_| DomainError::Capacity)?)
                .ok_or(DomainError::Capacity)?;
            if bytes > MAXIMUM_DOMAIN_BASELINE_BYTES {
                return Err(DomainError::Capacity);
            }
            self.validate_snapshot(snapshot)?;
        }
        for (id, entry) in &self.inner.definitions {
            if !seen.contains(id.as_str()) {
                return Err(DomainError::MissingState(entry.codec.identity().clone()));
            }
        }
        Ok(())
    }

    /// Validates the exact process-local issuer; a validated proposal cannot be relabeled.
    ///
    /// # Errors
    /// Returns `WrongGeneration` for another catalog or a withdrawn registration.
    pub fn validate_proposal<'a>(
        &self,
        proposal: &'a ValidatedDomainProposal,
    ) -> Result<&'a DomainStateValue> {
        if !Arc::ptr_eq(&self.inner.generation, &proposal.binding.generation)
            || !self
                .inner
                .definitions
                .get(proposal.snapshot.identity().id())
                .is_some_and(|entry| Arc::ptr_eq(entry, &proposal.binding.entry))
        {
            return Err(DomainError::WrongGeneration);
        }
        Ok(proposal.snapshot.state())
    }
}

impl Default for DomainCatalog {
    fn default() -> Self {
        Self {
            inner: Arc::new(Catalog {
                generation: Arc::new(()),
                definitions: BTreeMap::new(),
                baseline: Arc::from([]),
            }),
        }
    }
}

/// Process-local actual initial state for a draft or fresh admission.
/// Empty means the frozen empty set; a nonempty value supplies the sole baseline control.
#[derive(Clone, Debug)]
pub struct DomainBaseline {
    catalog: DomainCatalog,
    commit: Option<DomainStateCommit>,
}

impl DomainBaseline {
    pub(crate) fn ensure_catalog(&self, catalog: &DomainCatalog) -> Result<()> {
        if Arc::ptr_eq(&self.catalog.inner, &catalog.inner) {
            Ok(())
        } else {
            Err(DomainError::WrongGeneration)
        }
    }
    /// Freezes the defaults of the exact selected catalog.
    ///
    /// # Errors
    /// Rejects a baseline that cannot satisfy the canonical complete-request bounds.
    pub fn new(catalog: DomainCatalog) -> Result<Self> {
        let commit = baseline_commit(catalog.baseline().to_vec())?;
        Ok(Self { catalog, commit })
    }

    /// Replaces one typed initial value without creating a durable revision.
    ///
    /// # Errors
    /// Rejects a different generation, nonzero predecessor or aggregate capacity failure.
    /// Failure preserves the entire previous baseline.
    pub fn apply(&mut self, proposal: &ValidatedDomainProposal) -> Result<()> {
        self.catalog.validate_proposal(proposal)?;
        if proposal.expected_revision().get() != 0 {
            return Err(DomainError::BaselineRevision);
        }
        let mut snapshots = self.snapshots();
        let index = snapshots
            .binary_search_by(|snapshot| {
                snapshot
                    .identity()
                    .id()
                    .cmp(proposal.snapshot().identity().id())
            })
            .map_err(|_| DomainError::WrongGeneration)?;
        snapshots[index] = proposal.snapshot().clone();
        let next = baseline_commit(snapshots)?;
        self.commit = next;
        Ok(())
    }

    /// Overlays an exact historical set on this target catalog's defaults for a new fork.
    ///
    /// # Errors
    /// Rejects duplicate names, unsupported/invalid inherited state, or aggregate capacity.
    /// Failure preserves the candidate's prior initial states.
    pub fn inherit(&mut self, inherited: &[DomainSnapshot]) -> Result<()> {
        if inherited.len() > MAXIMUM_SESSION_DOMAINS {
            return Err(DomainError::Capacity);
        }
        let mut seen = std::collections::BTreeSet::new();
        let mut snapshots = self.snapshots();
        for snapshot in inherited {
            if !seen.insert(snapshot.identity().id()) {
                return Err(DomainError::Duplicate(snapshot.identity().id().into()));
            }
            self.catalog.validate_snapshot(snapshot)?;
            let index = snapshots
                .binary_search_by(|current| current.identity().id().cmp(snapshot.identity().id()))
                .map_err(|_| DomainError::Unsupported(snapshot.identity().clone()))?;
            snapshots[index] = snapshot.clone();
        }
        let next = baseline_commit(snapshots)?;
        self.commit = next;
        Ok(())
    }

    /// Returns the canonical initial-state request, or none for the empty set.
    pub const fn commit(&self) -> Option<&DomainStateCommit> {
        self.commit.as_ref()
    }

    /// Returns the baseline content identity; sixty-four zeroes denote the empty set.
    pub fn digest(&self) -> &str {
        self.commit.as_ref().map_or(
            "0000000000000000000000000000000000000000000000000000000000000000",
            DomainStateCommit::request_sha256,
        )
    }

    fn snapshots(&self) -> Vec<DomainSnapshot> {
        self.commit.as_ref().map_or_else(Vec::new, |commit| {
            commit
                .updates()
                .iter()
                .map(|update| update.snapshot().clone())
                .collect()
        })
    }
}

fn baseline_commit(snapshots: Vec<DomainSnapshot>) -> Result<Option<DomainStateCommit>> {
    if snapshots.is_empty() {
        return Ok(None);
    }
    let updates = snapshots
        .into_iter()
        .map(|snapshot| {
            DomainStateUpdate::new(DomainRevision::new(0), snapshot)
                .map_err(|_| DomainError::Capacity)
        })
        .collect::<Result<Vec<_>>>()?;
    DomainStateCommit::new(None, DomainMutationSource::Baseline, updates)
        .map(Some)
        .map_err(|_| DomainError::Capacity)
}

/// Typed proposal/decode handle for one exact frozen generation.
#[derive(Debug)]
pub struct DomainHandle<T> {
    definition: DomainDefinition<T>,
    binding: DomainBinding,
}

impl<T> Clone for DomainHandle<T> {
    fn clone(&self) -> Self {
        Self {
            definition: self.definition.clone(),
            binding: self.binding.clone(),
        }
    }
}

impl<T: Serialize + DeserializeOwned + Send + Sync + 'static> DomainHandle<T> {
    /// Validates a complete replacement and expected revision without choosing mutation provenance.
    ///
    /// # Errors
    /// Rejects semantic, encoding or structural failures as `InvalidState`.
    pub fn propose(
        &self,
        expected_revision: DomainRevision,
        value: &T,
    ) -> Result<ValidatedDomainProposal> {
        let identity = self.definition.identity();
        (self.definition.codec.validate)(value).map_err(|reason| invalid(identity, &reason))?;
        let state = encode(identity, value)?;
        self.definition.codec.validate(&state)?;
        Ok(ValidatedDomainProposal {
            binding: self.binding.clone(),
            expected_revision,
            snapshot: DomainSnapshot::new(identity.clone(), state),
        })
    }
    /// Decodes only the exact version owned by this typed handle.
    ///
    /// # Errors
    /// Rejects another codec version or invalid typed state.
    pub fn decode(&self, snapshot: &DomainSnapshot) -> Result<T> {
        if snapshot.identity() != self.definition.identity() {
            return Err(DomainError::Unsupported(snapshot.identity().clone()));
        }
        self.definition.codec.decode(snapshot.state())
    }
}

/// Semantically validated complete state with no caller-selectable authority or charging class.
#[derive(Clone, Debug)]
pub struct ValidatedDomainProposal {
    binding: DomainBinding,
    expected_revision: DomainRevision,
    snapshot: DomainSnapshot,
}

impl ValidatedDomainProposal {
    /// Returns the exact required predecessor revision.
    pub const fn expected_revision(&self) -> DomainRevision {
        self.expected_revision
    }
    /// Returns the opaque bounded replacement and its codec identity.
    pub const fn snapshot(&self) -> &DomainSnapshot {
        &self.snapshot
    }
}

fn invalid(identity: &DomainIdentity, reason: &str) -> DomainError {
    let mut end = reason
        .len()
        .min(rsi_agent_session_protocol::MAXIMUM_AGENT_DIAGNOSTIC_BYTES);
    while !reason.is_char_boundary(end) {
        end -= 1;
    }
    DomainError::InvalidState {
        domain: identity.id().into(),
        reason: reason[..end].into(),
    }
}

fn encode<T: Serialize>(identity: &DomainIdentity, value: &T) -> Result<DomainStateValue> {
    DomainStateValue::encode(value)
        .map_err(|_| invalid(identity, "state serialization or structural bounds failed"))
}
