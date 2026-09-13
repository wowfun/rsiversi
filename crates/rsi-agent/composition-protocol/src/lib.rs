//! Process-local Agent composition pins and fresh-session drafts.

#![deny(unsafe_code)]
#![warn(missing_docs)]

use async_trait::async_trait;
use rsi_agent_context::ModelContextBuilder;
use rsi_agent_session_protocol::{AgentPresetId, SessionHeader};
use rsi_meta_contract::LocalContract;
use rsi_tools_protocol::ToolRuntime;
use std::fmt;
use std::sync::Arc;
use thiserror::Error;

mod command;
mod contribution;
pub use command::{
    ContinuationCommand, DraftCommandError, DraftCommandMutation, DraftCommandPreparation,
    DraftCommandResult, MAXIMUM_DRAFT_COMMAND_RECEIPTS, PreparedDraftCommand, SessionCommand,
    SessionCommandContext, SessionCommandRegistration,
};
mod domain;
mod projection;
pub use contribution::{
    ContextContributor, ContributionBatch, ContributionCatalog, ContributionContext,
    ContributionError, ContributionFactPage, ContributionFactReader, ContributionHorizon,
    ContributionInput, ContributionKind, ContributionOutput, ContributionRegistrar,
    ContributionRegistrarContract, ContributionRegistration, ContributionResult, ContributionStage,
    MAXIMUM_AGENT_CONTRIBUTIONS, MAXIMUM_CONTRIBUTION_INPUT_BYTES, MAXIMUM_CONTRIBUTION_INPUTS,
    PostToolContributor, ToolPolicy, ToolPolicyDecision, ToolPolicyRequest,
};
pub use domain::{
    DomainBaseline, DomainBinding, DomainCatalog, DomainCatalogBuilder, DomainDefinition,
    DomainError, DomainHandle, DomainRegistrar, DomainRegistrarContract, DomainRegistration,
    ValidatedDomainProposal,
};
pub use projection::{SessionProjection, SessionProjectionAdapter, SessionProjectionContext};

/// Opaque lifetime owner retained by one composition pin.
pub trait AgentGenerationOwner: fmt::Debug + Send + Sync + 'static {}

impl<T> AgentGenerationOwner for T where T: fmt::Debug + Send + Sync + 'static {}

/// Exact immutable process-local Agent generation capability.
#[derive(Clone)]
pub struct AgentCompositionPin {
    preset_id: AgentPresetId,
    source_digest: String,
    tools: Arc<dyn ToolRuntime>,
    context_builder: Arc<dyn ModelContextBuilder>,
    domains: DomainCatalog,
    contributions: ContributionCatalog,
    owner: Arc<dyn AgentGenerationOwner>,
}

impl AgentCompositionPin {
    /// Creates a pin from one validated generation and its opaque owner.
    ///
    /// # Errors
    ///
    /// Returns [`AgentCompositionError::InvalidInput`] when `source_digest` is
    /// not lowercase SHA-256 hexadecimal.
    pub fn new(
        preset_id: AgentPresetId,
        source_digest: impl Into<String>,
        tools: Arc<dyn ToolRuntime>,
        context_builder: Arc<dyn ModelContextBuilder>,
        domains: DomainCatalog,
        contributions: ContributionCatalog,
        owner: Arc<dyn AgentGenerationOwner>,
    ) -> Result<Self> {
        let source_digest = source_digest.into();
        if source_digest.len() != 64
            || !source_digest
                .bytes()
                .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
        {
            return Err(AgentCompositionError::InvalidInput(
                "Agent composition source digest must be lowercase SHA-256 hex".into(),
            ));
        }
        Ok(Self {
            preset_id,
            source_digest,
            tools,
            context_builder,
            domains,
            contributions,
            owner,
        })
    }

    /// Returns the durable logical preset identity.
    pub const fn preset_id(&self) -> &AgentPresetId {
        &self.preset_id
    }

    /// Returns the effective source identity used to build this generation.
    ///
    /// The standing builder combines the Profile program and frozen executable
    /// catalog. This process-local identity is not a persisted artifact locator.
    pub fn source_digest(&self) -> &str {
        &self.source_digest
    }
    /// Compares exact linked-generation ownership, not merely identical source bytes.
    pub fn same_generation(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.owner, &other.owner)
    }

    /// Returns the immutable Tool Runtime pinned by this generation.
    pub fn tools(&self) -> Arc<dyn ToolRuntime> {
        Arc::clone(&self.tools)
    }

    /// Returns the unique immutable context builder from this exact generation.
    pub fn context_builder(&self) -> Arc<dyn ModelContextBuilder> {
        Arc::clone(&self.context_builder)
    }

    /// Returns the exact immutable domain definitions frozen with this generation.
    pub const fn domains(&self) -> &DomainCatalog {
        &self.domains
    }

    /// Returns callbacks captured and ordered with this exact generation.
    pub const fn contributions(&self) -> &ContributionCatalog {
        &self.contributions
    }
}

impl fmt::Debug for AgentCompositionPin {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AgentCompositionPin")
            .field("preset_id", &self.preset_id)
            .field("source_digest", &self.source_digest)
            .field("tools", &"<immutable Tool Runtime>")
            .field("context_builder", self.context_builder.identity())
            .finish_non_exhaustive()
    }
}

/// Standing Agent-generation resolver.
#[async_trait]
pub trait AgentComposition: fmt::Debug + Send + Sync + 'static {
    /// Reads the effective default identity from this resolver's catalog
    /// authority.
    ///
    /// # Errors
    ///
    /// Returns [`AgentCompositionError::DefaultUnavailable`] when the catalog's
    /// default adapter cannot be read, or [`AgentCompositionError::ShuttingDown`]
    /// after provider admission closes.
    async fn default_preset_id(&self) -> Result<AgentPresetId>;

    /// Resolves and pins the current healthy generation for one preset.
    ///
    /// # Errors
    ///
    /// Returns the closed [`AgentCompositionError`] class for invalid identity,
    /// unavailable source, exhausted capacity, or provider shutdown.
    async fn pin(&self, preset_id: &AgentPresetId) -> Result<AgentCompositionPin>;
}

/// Nominal Local contract for [`AgentComposition`].
#[derive(Debug)]
pub struct AgentCompositionContract;

impl LocalContract for AgentCompositionContract {
    const KEY: &'static str = "rsi.agent.composition";
    type Service = dyn AgentComposition;
}

/// Move-only fresh-session admission carrying the exact composition pin.
#[derive(Debug)]
pub struct PreparedFreshSession {
    inner: Box<PreparedFreshSessionInner>,
}

#[derive(Debug)]
struct PreparedFreshSessionInner {
    header: SessionHeader,
    composition: AgentCompositionPin,
    baseline: DomainBaseline,
}

impl PreparedFreshSession {
    /// Pairs a fresh header with its exact matching composition generation.
    ///
    /// # Errors
    ///
    /// Returns [`AgentCompositionError::InvalidInput`] when the Header and pin
    /// carry different preset identities.
    pub fn new(header: SessionHeader, composition: AgentCompositionPin) -> Result<Self> {
        if header.agent_preset_id() != composition.preset_id() {
            return Err(AgentCompositionError::InvalidInput(
                "fresh Session header and composition preset identities differ".into(),
            ));
        }
        let baseline = DomainBaseline::new(composition.domains().clone())?;
        Ok(Self {
            inner: Box::new(PreparedFreshSessionInner {
                header,
                composition,
                baseline,
            }),
        })
    }

    /// Returns the immutable candidate Header.
    pub const fn header(&self) -> &SessionHeader {
        &self.inner.header
    }

    /// Returns the exact candidate composition pin.
    pub const fn composition(&self) -> &AgentCompositionPin {
        &self.inner.composition
    }

    /// Returns the actual frozen initial states to be committed with first acceptance.
    pub const fn baseline(&self) -> &DomainBaseline {
        &self.inner.baseline
    }

    /// Selects already prepared initial state from the exact same generation.
    ///
    /// # Errors
    /// Rejects a baseline from another generation.
    pub fn with_baseline(mut self, baseline: DomainBaseline) -> Result<Self> {
        baseline.ensure_catalog(self.inner.composition.domains())?;
        self.inner.baseline = baseline;
        Ok(self)
    }

    /// Consumes the fresh admission into its exact owned parts.
    pub fn into_parts(self) -> (SessionHeader, AgentCompositionPin, DomainBaseline) {
        let inner = *self.inner;
        (inner.header, inner.composition, inner.baseline)
    }
}

/// Process-local empty-session draft that has not created Store state.
#[derive(Debug)]
pub struct AgentSessionDraft {
    identity: Arc<()>,
    revision: u64,
    command_receipts: std::collections::BTreeMap<
        rsi_agent_session_protocol::DomainRequestId,
        rsi_agent_session_protocol::SessionCommandReceipt,
    >,
    header: SessionHeader,
    composition_service: Arc<dyn AgentComposition>,
    composition: AgentCompositionPin,
    baseline: DomainBaseline,
}

/// Move-only replacement generation prepared for one exact draft predecessor.
#[derive(Debug)]
pub struct PreparedDraftPreset {
    identity: Arc<()>,
    expected_revision: u64,
    header: SessionHeader,
    composition: AgentCompositionPin,
    baseline: DomainBaseline,
}

impl AgentSessionDraft {
    /// Resolves the Header's initial preset into one draft pin.
    ///
    /// # Errors
    ///
    /// Propagates composition resolution failure or rejects a service result
    /// carrying a different preset identity.
    pub async fn new(
        header: SessionHeader,
        composition_service: Arc<dyn AgentComposition>,
    ) -> Result<Self> {
        let composition = composition_service.pin(header.agent_preset_id()).await?;
        if composition.preset_id() != header.agent_preset_id() {
            return Err(AgentCompositionError::InvalidInput(
                "Agent composition returned a different preset identity".into(),
            ));
        }
        let baseline = DomainBaseline::new(composition.domains().clone())?;
        Ok(Self {
            header,
            composition_service,
            composition,
            baseline,
            identity: Arc::new(()),
            revision: 0,
            command_receipts: std::collections::BTreeMap::new(),
        })
    }

    /// Returns the actual candidate Header, including the currently selected preset.
    pub const fn header(&self) -> &SessionHeader {
        &self.header
    }

    /// Returns the currently selected logical preset identity.
    pub const fn agent_preset_id(&self) -> &AgentPresetId {
        self.header.agent_preset_id()
    }

    /// Returns the exact currently staged generation.
    pub const fn composition(&self) -> &AgentCompositionPin {
        &self.composition
    }

    /// Returns the current process-local initial states.
    pub const fn baseline(&self) -> &DomainBaseline {
        &self.baseline
    }

    /// Applies a typed initial-state proposal without writing a Header or control.
    ///
    /// # Errors
    /// Rejects wrong generations, nonzero revisions and aggregate bound violations.
    pub fn apply_domain_initial(&mut self, proposal: &ValidatedDomainProposal) -> Result<()> {
        self.apply_domain_initial_batch(std::slice::from_ref(proposal))
    }

    /// Applies a complete initial-state batch without publishing a successful prefix.
    ///
    /// # Errors
    /// Rejects invalid generation, revision, duplicate domain or aggregate bounds.
    pub fn apply_domain_initial_batch(
        &mut self,
        proposals: &[ValidatedDomainProposal],
    ) -> Result<()> {
        if proposals.is_empty() {
            return Ok(());
        }
        let revision = self.next_revision()?;
        self.baseline.apply_batch(proposals)?;
        self.revision = revision;
        Ok(())
    }

    fn next_revision(&self) -> Result<u64> {
        self.revision
            .checked_add(1)
            .ok_or_else(|| AgentCompositionError::InvalidInput("draft revision exhausted".into()))
    }

    /// Freezes the actual draft payload for one first-publication attempt.
    /// The lease owner serializes this operation with mutations and publication.
    pub fn freeze(&self) -> PreparedFreshSession {
        PreparedFreshSession {
            inner: Box::new(PreparedFreshSessionInner {
                header: self.header.clone(),
                composition: self.composition.clone(),
                baseline: self.baseline.clone(),
            }),
        }
    }

    /// Fully stages and then atomically selects one replacement preset.
    ///
    /// # Errors
    ///
    /// Propagates composition resolution failure or rejects a service result
    /// carrying a different preset identity. Failure leaves the draft intact.
    pub async fn select_preset(&mut self, preset_id: AgentPresetId) -> Result<()> {
        let prepared = self.prepare_preset_selection(preset_id).await?;
        self.apply_preset_selection(prepared)
            .map_err(|error| AgentCompositionError::InvalidInput(error.to_string()))
    }

    /// Captures an owned generation-preparation future without holding mutation admission.
    ///
    /// # Errors
    /// The future rejects exhausted revisions, unavailable or mismatched generations.
    pub fn prepare_preset_selection(
        &self,
        preset_id: AgentPresetId,
    ) -> impl std::future::Future<Output = Result<PreparedDraftPreset>> + Send + 'static {
        let next = self.next_revision();
        let expected_revision = self.revision;
        let identity = self.identity.clone();
        let service = self.composition_service.clone();
        let header = self.header.clone();
        async move {
            next?;
            let composition = service.pin(&preset_id).await?;
            if composition.preset_id() != &preset_id {
                return Err(AgentCompositionError::InvalidInput(
                    "Agent composition returned a different preset identity".into(),
                ));
            }
            let header = header
                .with_agent_preset_id(preset_id)
                .map_err(|error| AgentCompositionError::InvalidInput(error.to_string()))?;
            let baseline = DomainBaseline::new(composition.domains().clone())?;
            Ok(PreparedDraftPreset {
                identity,
                expected_revision,
                header,
                composition,
                baseline,
            })
        }
    }

    /// Atomically selects a prepared Header, pin and defaults for the same draft predecessor.
    ///
    /// # Errors
    /// Rejects another draft or any intervening mutation, preserving the current payload.
    pub fn apply_preset_selection(
        &mut self,
        prepared: PreparedDraftPreset,
    ) -> DraftCommandResult<()> {
        use rsi_agent_session_protocol::CommandRevision;
        if !Arc::ptr_eq(&self.identity, &prepared.identity) {
            return Err(DraftCommandError::WrongDraft);
        }
        if self.revision != prepared.expected_revision {
            return Err(DraftCommandError::Revision {
                expected: CommandRevision::Draft {
                    revision: prepared.expected_revision,
                },
                actual: self.revision(),
            });
        }
        self.header = prepared.header;
        self.composition = prepared.composition;
        self.baseline = prepared.baseline;
        self.revision = prepared.expected_revision + 1;
        Ok(())
    }

    /// Consumes this draft into one fresh-session admission.
    pub fn into_fresh(self) -> PreparedFreshSession {
        PreparedFreshSession {
            inner: Box::new(PreparedFreshSessionInner {
                header: self.header,
                composition: self.composition,
                baseline: self.baseline,
            }),
        }
    }
}

/// Closed composition failure taxonomy.
#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum AgentCompositionError {
    /// Typed initial-state validation or binding failed.
    #[error(transparent)]
    Domain(#[from] DomainError),
    /// Malformed or internally inconsistent bounded input.
    #[error("invalid Agent composition input: {0}")]
    InvalidInput(String),
    /// The requested preset cannot currently produce a healthy generation.
    #[error("Agent preset {preset_id} is unavailable: {reason}")]
    Unavailable {
        /// Exact logical preset identity.
        preset_id: AgentPresetId,
        /// Bounded safe diagnostic.
        reason: String,
    },
    /// The catalog's effective default cannot currently be read.
    #[error("default Agent preset is unavailable: {reason}")]
    DefaultUnavailable {
        /// Bounded safe diagnostic.
        reason: String,
    },
    /// Standing generation or build admission is exhausted.
    #[error("Agent composition capacity is exhausted")]
    Capacity,
    /// The composition owner no longer admits new generations.
    #[error("Agent composition is shutting down")]
    ShuttingDown,
}

/// Composition result.
pub type Result<T> = std::result::Result<T, AgentCompositionError>;
