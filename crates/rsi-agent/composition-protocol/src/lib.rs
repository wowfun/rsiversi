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

mod domain;
pub use domain::{
    DomainBaseline, DomainBinding, DomainCatalog, DomainCatalogBuilder, DomainDefinition,
    DomainError, DomainHandle, DomainRegistrar, DomainRegistrarContract, DomainRegistration,
    ValidatedDomainProposal,
};

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
    _owner: Arc<dyn AgentGenerationOwner>,
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
            _owner: owner,
        })
    }

    /// Returns the durable logical preset identity.
    pub const fn preset_id(&self) -> &AgentPresetId {
        &self.preset_id
    }

    /// Returns the exact source digest used to build this generation.
    pub fn source_digest(&self) -> &str {
        &self.source_digest
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
    header: SessionHeader,
    composition_service: Arc<dyn AgentComposition>,
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
        })
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
        self.baseline.apply(proposal)?;
        Ok(())
    }

    /// Fully stages and then atomically selects one replacement preset.
    ///
    /// # Errors
    ///
    /// Propagates composition resolution failure or rejects a service result
    /// carrying a different preset identity. Failure leaves the draft intact.
    pub async fn select_preset(&mut self, preset_id: AgentPresetId) -> Result<()> {
        let composition = self.composition_service.pin(&preset_id).await?;
        if composition.preset_id() != &preset_id {
            return Err(AgentCompositionError::InvalidInput(
                "Agent composition returned a different preset identity".into(),
            ));
        }
        let header = self
            .header
            .clone()
            .with_agent_preset_id(preset_id)
            .map_err(|error| AgentCompositionError::InvalidInput(error.to_string()))?;
        let baseline = DomainBaseline::new(composition.domains().clone())?;
        self.header = header;
        self.composition = composition;
        self.baseline = baseline;
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
