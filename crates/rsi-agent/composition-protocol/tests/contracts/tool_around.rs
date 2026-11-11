use super::*;
use rsi_agent_composition_protocol::{
    ContributionCatalog, ContributionKind, ContributionRegistration, DomainCatalog,
    ToolAroundContributor, ToolContinuation,
};

#[derive(Debug)]
struct Pass;

#[async_trait]
impl ToolAroundContributor for Pass {
    async fn execute(
        &self,
        next: ToolContinuation<'_>,
    ) -> rsi_tools_protocol::Result<rsi_tools_protocol::ToolResult> {
        next.run().await
    }
}

#[tokio::test]
async fn native_around_plan_keeps_its_generation_until_release() {
    let runtime = rsi_meta::Runtime::default();
    let (_, context) = rsi_agent_testkit::activate_contribution_owner(&runtime.root())
        .await
        .unwrap();
    let (position, lease) = context
        .registration_context()
        .unwrap()
        .register("around fixture", || Ok(()), Ok)
        .unwrap();
    let catalog = ContributionCatalog::freeze(vec![(
        ContributionRegistration::new(
            rsi_agent_session_protocol::ContributionId::new("fixture.around").unwrap(),
            0,
            ContributionKind::ToolAround(Arc::new(Pass)),
        ),
        position,
    )])
    .unwrap();
    let owner = Arc::new(GenerationOwner);
    let generation = Arc::downgrade(&owner);
    let pin = AgentCompositionPin::new(
        AgentPresetId::new("alpha").unwrap(),
        "a".repeat(64),
        Arc::new(EmptyTools::default()),
        Arc::new(rsi_agent_context::DefaultContextBuilder::default()),
        DomainCatalog::default(),
        catalog,
        owner,
    )
    .unwrap();
    let call = ToolCall {
        id: "call".into(),
        name: "echo".into(),
        arguments: serde_json::json!({}),
    };
    let mut continuation = None;
    pin.wrap_tool_execution(&mut rsi_tools_protocol::NativeToolPlan {
        call: &call,
        output: None,
        continuation: &mut continuation,
    });
    drop(pin);
    let retained = generation.upgrade().is_some();
    drop(continuation);
    let released = generation.upgrade().is_none();
    drop(lease);
    assert!(runtime.shutdown().await.is_clean());
    assert!(
        retained,
        "dropping the request pin must not retire an admitted hook's generation"
    );
    assert!(
        released,
        "releasing the last plan must release its generation"
    );
}
