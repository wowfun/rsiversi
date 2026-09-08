use async_trait::async_trait;
use rsi_agent_composition_protocol::{
    AgentComposition, AgentCompositionError, AgentCompositionPin, AgentSessionDraft,
};
use rsi_agent_session_protocol::{
    AgentPresetId, FrozenAgentSettings, SessionHeader, SessionId, TurnBudget,
};
use rsi_ai_protocol::ModelRef;
use rsi_sandbox::SandboxMode;
use rsi_tools_protocol::{
    PreparedToolCall, RetainedToolResult, ToolCall, ToolDefinition, ToolResultIdentity, ToolRuntime,
};
use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};
use tokio_util::sync::CancellationToken;

#[derive(Debug)]
struct EmptyTools;

#[async_trait]
impl ToolRuntime for EmptyTools {
    fn definitions(&self) -> Vec<ToolDefinition> {
        Vec::new()
    }

    fn prepare(
        &self,
        _invocation_id: &str,
        call: ToolCall,
    ) -> rsi_tools_protocol::Result<Box<dyn PreparedToolCall>> {
        Err(rsi_tools_protocol::ToolError::Unknown(call.name))
    }

    fn query(
        &self,
        _identity: &ToolResultIdentity,
    ) -> rsi_tools_protocol::Result<RetainedToolResult> {
        Ok(RetainedToolResult::Absent)
    }

    async fn wait(
        &self,
        _identity: &ToolResultIdentity,
        _cancellation: CancellationToken,
    ) -> rsi_tools_protocol::Result<RetainedToolResult> {
        Ok(RetainedToolResult::Absent)
    }

    fn commit(&self, _identity: &ToolResultIdentity) -> rsi_tools_protocol::Result<()> {
        Err(rsi_tools_protocol::ToolError::InvalidInput("absent".into()))
    }
}

#[derive(Debug)]
struct GenerationOwner;

#[derive(Debug)]
struct FakeComposition {
    failures: Mutex<BTreeSet<AgentPresetId>>,
    domains: rsi_agent_composition_protocol::DomainCatalog,
}

#[async_trait]
impl AgentComposition for FakeComposition {
    async fn default_preset_id(&self) -> rsi_agent_composition_protocol::Result<AgentPresetId> {
        Ok(AgentPresetId::new("alpha").unwrap())
    }

    async fn pin(
        &self,
        preset_id: &AgentPresetId,
    ) -> rsi_agent_composition_protocol::Result<AgentCompositionPin> {
        if self.failures.lock().unwrap().contains(preset_id) {
            return Err(AgentCompositionError::Unavailable {
                preset_id: preset_id.clone(),
                reason: "broken candidate".into(),
            });
        }
        AgentCompositionPin::new(
            preset_id.clone(),
            "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
            Arc::new(EmptyTools),
            Arc::new(rsi_agent_context::DefaultContextBuilder::default()),
            self.domains.clone(),
            rsi_agent_composition_protocol::ContributionCatalog::default(),
            Arc::new(GenerationOwner),
        )
    }
}

fn header(preset_id: &str) -> SessionHeader {
    SessionHeader::new(
        SessionId::new("session-1").unwrap(),
        1,
        "/workspace",
        AgentPresetId::new(preset_id).unwrap(),
        FrozenAgentSettings::new_with_budget(
            "profile",
            "system",
            ModelRef::new("provider", "model").unwrap(),
            SandboxMode::ReadOnly,
            true,
            TurnBudget::default(),
        )
        .unwrap(),
    )
    .unwrap()
}

#[tokio::test]
async fn failed_switch_preserves_the_exact_prior_draft_and_success_moves_one_pin() {
    let composition = Arc::new(FakeComposition {
        failures: Mutex::new(BTreeSet::from([AgentPresetId::new("broken").unwrap()])),
        domains: rsi_agent_composition_protocol::DomainCatalog::default(),
    });
    assert_eq!(
        composition.default_preset_id().await.unwrap().as_str(),
        "alpha"
    );
    let mut draft = AgentSessionDraft::new(header("alpha"), composition)
        .await
        .unwrap();

    assert!(
        draft
            .select_preset(AgentPresetId::new("broken").unwrap())
            .await
            .is_err()
    );
    assert_eq!(draft.agent_preset_id().as_str(), "alpha");
    assert_eq!(draft.composition().preset_id().as_str(), "alpha");

    draft
        .select_preset(AgentPresetId::new("beta").unwrap())
        .await
        .unwrap();
    let fresh = draft.into_fresh();
    assert_eq!(fresh.header().agent_preset_id().as_str(), "beta");
    assert_eq!(fresh.composition().preset_id().as_str(), "beta");
    let (header, pin, baseline) = fresh.into_parts();
    assert_eq!(header.agent_preset_id(), pin.preset_id());
    assert!(baseline.commit().is_none());
}

#[tokio::test]
async fn freezing_and_failed_switch_preserve_mutated_initial_state_but_success_resets_defaults() {
    use rsi_agent_composition_protocol::{DomainCatalog, DomainDefinition};
    use rsi_agent_session_protocol::{DomainIdentity, DomainRevision};
    let definition =
        DomainDefinition::new(DomainIdentity::new("plan", 1).unwrap(), &false, |_| Ok(())).unwrap();
    let domains = DomainCatalog::new([definition.registration()]).unwrap();
    let handle = domains.bind(&definition).unwrap();
    let composition = Arc::new(FakeComposition {
        failures: Mutex::new(BTreeSet::from([AgentPresetId::new("broken").unwrap()])),
        domains,
    });
    let mut draft = AgentSessionDraft::new(header("alpha"), composition)
        .await
        .unwrap();
    let default_digest = draft.baseline().digest().to_owned();
    draft
        .apply_domain_initial_batch(&[handle.propose(DomainRevision::new(0), &true).unwrap()])
        .unwrap();
    let frozen = draft.freeze();
    assert_ne!(frozen.baseline().digest(), default_digest);
    assert_eq!(frozen.baseline().digest(), draft.baseline().digest());
    assert!(
        draft
            .select_preset(AgentPresetId::new("broken").unwrap())
            .await
            .is_err()
    );
    assert_eq!(
        frozen.baseline().digest(),
        draft.freeze().baseline().digest()
    );
    draft
        .select_preset(AgentPresetId::new("beta").unwrap())
        .await
        .unwrap();
    assert_eq!(draft.baseline().digest(), default_digest);
    assert_ne!(frozen.baseline().digest(), default_digest);
    assert_eq!(frozen.header().agent_preset_id().as_str(), "alpha");
    assert_eq!(draft.freeze().header().agent_preset_id().as_str(), "beta");
}

#[test]
fn pin_rejects_non_sha256_source_identity() {
    assert!(matches!(
        AgentCompositionPin::new(
            AgentPresetId::new("alpha").unwrap(),
            "not-a-digest",
            Arc::new(EmptyTools),
            Arc::new(rsi_agent_context::DefaultContextBuilder::default()),
            rsi_agent_composition_protocol::DomainCatalog::default(),
            rsi_agent_composition_protocol::ContributionCatalog::default(),
            Arc::new(GenerationOwner),
        ),
        Err(AgentCompositionError::InvalidInput(_))
    ));
}

#[tokio::test]
async fn owned_preset_preparation_cannot_overwrite_a_later_selection_or_another_draft() {
    let composition = Arc::new(FakeComposition {
        failures: Mutex::new(BTreeSet::new()),
        domains: rsi_agent_composition_protocol::DomainCatalog::default(),
    });
    let mut draft = AgentSessionDraft::new(header("alpha"), composition.clone())
        .await
        .unwrap();
    let pending = draft.prepare_preset_selection(AgentPresetId::new("beta").unwrap());
    draft
        .select_preset(AgentPresetId::new("gamma").unwrap())
        .await
        .unwrap();
    assert!(matches!(
        draft.apply_preset_selection(pending.await.unwrap()),
        Err(rsi_agent_composition_protocol::DraftCommandError::Revision { .. })
    ));
    assert_eq!(draft.header().agent_preset_id().as_str(), "gamma");
    let mut other = AgentSessionDraft::new(header("alpha"), composition)
        .await
        .unwrap();
    let prepared = draft
        .prepare_preset_selection(AgentPresetId::new("beta").unwrap())
        .await
        .unwrap();
    assert!(matches!(
        other.apply_preset_selection(prepared),
        Err(rsi_agent_composition_protocol::DraftCommandError::WrongDraft)
    ));
    assert_eq!(other.header().agent_preset_id().as_str(), "alpha");
}
