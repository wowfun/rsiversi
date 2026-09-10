use super::{
    ContextContributor, ContributionError, ContributionResult, MAXIMUM_AGENT_CONTRIBUTIONS,
    PostToolContributor, ToolPolicy,
};
use rsi_agent_session_protocol::ContributionId;
use rsi_meta::{
    RegistrationContext, RegistrationLease, RegistrationOrderSnapshot, RegistrationPosition,
};
use rsi_meta_contract::LocalContract;
use std::{collections::BTreeSet, fmt, sync::Arc};

/// Closed framework execution stages.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ContributionStage {
    /// Before a new provider retry series.
    BeforeStep,
    /// After source-ordered Tool settlement.
    AfterTools,
    /// Before Tool approval or execution.
    ToolPolicy,
    /// Explicit Session command dispatch, outside the execution loop.
    Command,
    /// Disposable extension view over a captured Session state.
    Projection,
}

/// One narrow callback registered in an Agent-only composition.
#[derive(Clone, Debug)]
pub enum ContributionKind {
    /// Model context input producer.
    Context(Arc<dyn ContextContributor>),
    /// Consumer of a durably settled Tool batch.
    PostTool(Arc<dyn PostToolContributor>),
    /// Monotone prepared-Tool policy.
    ToolPolicy(Arc<dyn ToolPolicy>),
    /// Effect-free Session command with bounded discovery metadata.
    Command(crate::SessionCommandRegistration),
    /// Read-only complete Session view, isolated from other projection failures.
    Projection(Arc<dyn crate::SessionProjection>),
}

/// Stable identity, explicit priority and one callback.
#[derive(Clone, Debug)]
pub struct ContributionRegistration {
    id: ContributionId,
    priority: i32,
    kind: ContributionKind,
}

impl ContributionRegistration {
    /// Declares a contribution without acquiring lifecycle authority.
    pub const fn new(id: ContributionId, priority: i32, kind: ContributionKind) -> Self {
        Self { id, priority, kind }
    }
    /// Returns stable business identity, the final ordering tie break.
    pub const fn id(&self) -> &ContributionId {
        &self.id
    }
    /// Returns explicit primary sort priority (lower values run first).
    pub const fn priority(&self) -> i32 {
        self.priority
    }
    /// Borrows the exact frozen callback.
    pub const fn kind(&self) -> &ContributionKind {
        &self.kind
    }
    /// Returns its framework stage.
    pub const fn stage(&self) -> ContributionStage {
        match self.kind {
            ContributionKind::Context(_) => ContributionStage::BeforeStep,
            ContributionKind::PostTool(_) => ContributionStage::AfterTools,
            ContributionKind::ToolPolicy(_) => ContributionStage::ToolPolicy,
            ContributionKind::Command(_) => ContributionStage::Command,
            ContributionKind::Projection(_) => ContributionStage::Projection,
        }
    }
}

/// Immutable callbacks captured with the same generation as Tools and domain codecs.
#[derive(Clone, Debug, Default)]
pub struct ContributionCatalog {
    entries: Arc<[ContributionRegistration]>,
}

impl ContributionCatalog {
    /// Freezes admitting registrations at one composition-order publication boundary.
    /// Existing pins retain this captured order and callbacks after owner withdrawal.
    ///
    /// # Errors
    /// Rejects duplicate identities, capacity, or positions from different Runtimes.
    pub fn freeze(
        mut entries: Vec<(ContributionRegistration, RegistrationPosition)>,
    ) -> ContributionResult<Self> {
        if entries.len() > MAXIMUM_AGENT_CONTRIBUTIONS {
            return Err(ContributionError::Capacity);
        }
        entries.retain(|(_, position)| position.is_admitting());
        let mut ids = BTreeSet::new();
        let mut command_names = BTreeSet::new();
        for (entry, _) in &entries {
            if !ids.insert(entry.id()) {
                return Err(ContributionError::Duplicate(entry.id().clone()));
            }
            if let ContributionKind::Command(command) = entry.kind()
                && (command.descriptor().id() != entry.id()
                    || !command_names.insert(command.descriptor().name()))
            {
                return Err(ContributionError::Invalid(
                    "command identity mismatch or duplicate name".into(),
                ));
            }
        }
        let positions: Vec<_> = entries
            .iter()
            .map(|(_, position)| position.clone())
            .collect();
        let snapshot = RegistrationOrderSnapshot::capture(&positions)
            .map_err(|_| ContributionError::RegistrationUnavailable)?;
        let mut ranked: Vec<_> = entries.into_iter().zip(snapshot.ranks()).collect();
        ranked.sort_by(|((a, _), rank_a), ((b, _), rank_b)| {
            a.priority
                .cmp(&b.priority)
                .then_with(|| rank_a.compare_position(rank_b))
                .then_with(|| a.id.cmp(&b.id))
        });
        Ok(Self {
            entries: ranked.into_iter().map(|((entry, _), _)| entry).collect(),
        })
    }
    /// Borrows all callbacks in frozen execution order.
    pub fn entries(&self) -> &[ContributionRegistration] {
        &self.entries
    }
}

/// Write-only callback admission for one unpublished Agent generation.
pub trait ContributionRegistrar: fmt::Debug + Send + Sync + 'static {
    /// Installs exact undo before publication under the caller's Meta generation.
    ///
    /// # Errors
    /// Rejects closed stages, duplicates, capacity or unavailable registration ownership.
    fn register(
        &self,
        context: &RegistrationContext,
        contribution: ContributionRegistration,
    ) -> ContributionResult<RegistrationLease>;
}

/// Nominal Local contract for the unpublished callback registrar.
#[derive(Debug)]
pub struct ContributionRegistrarContract;
impl LocalContract for ContributionRegistrarContract {
    const KEY: &'static str = "rsi.agent.contribution.registrar";
    type Service = dyn ContributionRegistrar;
}
