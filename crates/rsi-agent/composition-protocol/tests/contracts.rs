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
    let (header, pin) = fresh.into_parts();
    assert_eq!(header.agent_preset_id(), pin.preset_id());
}

#[test]
fn pin_rejects_non_sha256_source_identity() {
    assert!(matches!(
        AgentCompositionPin::new(
            AgentPresetId::new("alpha").unwrap(),
            "not-a-digest",
            Arc::new(EmptyTools),
            Arc::new(GenerationOwner),
        ),
        Err(AgentCompositionError::InvalidInput(_))
    ));
}
