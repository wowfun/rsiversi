use rsi_agent_composition::{
    AgentCompositionFactory, AgentContributionCatalog, AgentGenerationRootFactory,
};
use rsi_agent_composition_protocol::{
    AgentComposition, AgentCompositionContract, AgentSessionDraft, ContributionKind,
    DraftCommandPreparation, SessionProjectionAdapter, SessionProjectionContext,
};
use rsi_agent_presets::{
    AgentPresetCatalog, AgentPresetCatalogConfig, AgentPresetProfileCompiler, AgentPresetRoot,
    AgentPresetTrust,
};
use rsi_agent_session_protocol::{
    AgentPresetId, CommandArguments, CommandRevision, ContributionId, DomainRequestId,
    DomainRevision, DomainStateView, FrozenAgentSettings, ProjectionCursor,
    SessionCommandInvocation, SessionHeader, SessionId,
};
use rsi_meta::{ConfigValue, PluginFactory, ResolvedFactory, Runtime, UpdateMode};
use rsi_meta_profile::{ProfileCompiler, ProfileEnvironment, ProfileLimits};
use rsi_meta_scope::ScopeRoot;
use rsi_sandbox::SandboxMode;
use std::{collections::BTreeMap, sync::Arc};
use tokio_util::sync::CancellationToken;

struct Fixture {
    temp: tempfile::TempDir,
    runtime: Runtime,
    composition: Arc<dyn AgentComposition>,
}
fn linked(id: &str, factory: impl PluginFactory) -> ResolvedFactory {
    ResolvedFactory::linked(
        id,
        "fixture",
        UpdateMode::RestartRequired,
        Arc::new(factory),
    )
}
impl Fixture {
    async fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let presets_root = temp.path().join("presets");
        let preset = presets_root.join("fixture");
        std::fs::create_dir_all(&preset).unwrap();
        std::fs::write(preset.join("agent.profile.toml"), "format = 1\n[[steps]]\nkind = \"plugin\"\nid = \"context\"\nplugin = \"fixture.context\"\n[[steps]]\nkind = \"plugin\"\nid = \"plan\"\nplugin = \"fixture.goal\"\n").unwrap();
        for name in ["config", "state", "cache"] {
            std::fs::create_dir_all(temp.path().join(name)).unwrap();
        }
        let environment = ProfileEnvironment::new(
            temp.path().join("config"),
            temp.path().join("state"),
            temp.path().join("cache"),
            "fixture",
            BTreeMap::new(),
        )
        .unwrap();
        let presets = AgentPresetCatalog::new(
            AgentPresetCatalogConfig::new(AgentPresetId::new("fixture").unwrap())
                .with_configured_root(
                    AgentPresetRoot::new(presets_root, AgentPresetTrust::User).unwrap(),
                ),
            AgentPresetProfileCompiler::new(
                ProfileCompiler::new(environment, ProfileLimits::default()),
                ["fixture.context", "fixture.goal"],
            ),
        )
        .unwrap();
        let contributions = AgentContributionCatalog::new([
            linked(
                "fixture.context",
                rsi_agent_context::DefaultContextBuilderFactory,
            ),
            linked("fixture.goal", rsi_agent_goal::GoalFactory),
        ])
        .unwrap();
        let runtime = Runtime::default();
        for factory in [
            linked("fixture.tools", rsi_tools::ToolsFactory),
            linked("fixture.root", AgentGenerationRootFactory),
            linked(
                "fixture.composition",
                AgentCompositionFactory::new(presets, contributions, ScopeRoot::new(16).unwrap()),
            ),
        ] {
            runtime
                .root()
                .apply(factory, ConfigValue::Null)
                .await
                .unwrap();
        }
        let composition = runtime
            .root()
            .lookup_local::<AgentCompositionContract>()
            .unwrap();
        Self {
            temp,
            runtime,
            composition,
        }
    }
    fn header(&self) -> SessionHeader {
        SessionHeader::new(
            SessionId::new("goal-fixture").unwrap(),
            1,
            self.temp.path().to_str().unwrap(),
            AgentPresetId::new("fixture").unwrap(),
            FrozenAgentSettings::new(
                "settings",
                "system",
                rsi_ai_protocol::ModelRef::new("fixture", "model").unwrap(),
                SandboxMode::WorkspaceWrite,
                false,
            )
            .unwrap(),
        )
        .unwrap()
    }
    async fn draft(&self) -> AgentSessionDraft {
        AgentSessionDraft::new(self.header(), self.composition.clone())
            .await
            .unwrap()
    }
    async fn stop(self) {
        assert!(self.runtime.shutdown().await.is_clean());
    }
}

async fn create(draft: &mut AgentSessionDraft) {
    let invocation = SessionCommandInvocation {
        command: ContributionId::new(rsi_agent_goal::GOAL_COMMAND).unwrap(),
        request_id: DomainRequestId::new("create-request").unwrap(), expected_revision: draft.revision(),
        arguments: CommandArguments::new(serde_json::json!({"action":"create","id":"goal-task","objective":"Implement and test the task","constraints":"Preserve unrelated files","max_rounds":3})).unwrap(),
    };
    let DraftCommandPreparation::Run(prepared) = draft.prepare_command(invocation).unwrap() else {
        panic!("new command");
    };
    let mutation = prepared.execute(CancellationToken::new()).await.unwrap();
    draft.apply_command(mutation).unwrap();
}

async fn view(draft: &AgentSessionDraft) -> serde_json::Value {
    let CommandRevision::Draft { revision } = draft.revision() else {
        panic!("draft revision");
    };
    let context = SessionProjectionContext::new(
        Arc::new(draft.header().clone()),
        ProjectionCursor::Draft { revision },
        draft
            .baseline()
            .initial_states()
            .into_iter()
            .map(|snapshot| DomainStateView {
                revision: DomainRevision::new(0),
                snapshot,
            })
            .collect(),
    )
    .unwrap();
    let snapshot = SessionProjectionAdapter::new(draft.composition().clone())
        .snapshot(
            &context,
            &rsi_meta::Execution::native(tokio::runtime::Handle::current()),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(snapshot.entries().len(), 1);
    snapshot.entries()[0].view().unwrap().value().clone()
}

#[tokio::test]
async fn linked_factory_contributes_typed_state_pure_callbacks_and_a_bounded_report_tool() {
    let fixture = Fixture::new().await;
    let mut draft = fixture.draft().await;
    assert_eq!(view(&draft).await, serde_json::json!({"goal":null}));
    assert_eq!(draft.command_descriptors().len(), 1);
    assert_eq!(draft.command_descriptors()[0].name(), "goal");
    let tools = draft.composition().tools();
    assert_eq!(tools.definitions().len(), 1);
    assert_eq!(tools.definitions()[0].name(), "report_goal");
    assert!(
        tools.definitions()[0]
            .description()
            .contains("alone as the final Tool call")
    );
    for id in [rsi_agent_goal::GOAL_RESERVE, rsi_agent_goal::GOAL_SETTLE] {
        let invocation = SessionCommandInvocation {
            command: ContributionId::new(id).unwrap(),
            request_id: DomainRequestId::new("forged").unwrap(),
            expected_revision: draft.revision(),
            arguments: CommandArguments::new(serde_json::json!({"id":"goal-task"})).unwrap(),
        };
        assert!(draft.prepare_command(invocation).is_err());
    }
    create(&mut draft).await;
    let projected = view(&draft).await;
    assert_eq!(projected["goal"]["allocated_rounds"], 1);
    assert_eq!(projected["goal"]["max_rounds"], 3);
    for (index, args) in [serde_json::json!({"goal_id":"goal-task","kind":"resume","evidence":"keep running"}), serde_json::json!({"goal_id":"goal-task","kind":"complete","evidence":"ok","max_rounds":999})].into_iter().enumerate() {
        let prepared = tools.prepare(&format!("invalid-report-{index}"), rsi_tools_protocol::ToolCall { id: "call".into(), name: "report_goal".into(), arguments: args }).unwrap();
        let identity = prepared.identity().clone();
        assert!(prepared.start(tool_start(&fixture)).await.is_err());
        tools.commit(&identity).unwrap();
    }
    let prepared = tools.prepare("valid-report", rsi_tools_protocol::ToolCall { id: "call".into(), name: "report_goal".into(), arguments: serde_json::json!({"goal_id":"goal-task","kind":"complete","evidence":"All independent acceptance checks passed"}) }).unwrap();
    let identity = prepared.identity().clone();
    let result = prepared.start(tool_start(&fixture)).await.unwrap();
    assert_eq!(result.value["goal_report"]["version"], 1);
    assert!(!result.content.is_empty());
    assert!(result.enforcement.is_empty());
    tools.commit(&identity).unwrap();
    drop((tools, draft));
    fixture.stop().await;
}

#[tokio::test]
async fn reserve_callback_rejects_any_input_differing_from_its_proposed_allocation() {
    let fixture = Fixture::new().await;
    let mut draft = fixture.draft().await;
    create(&mut draft).await;
    let mut goal: rsi_agent_goal::GoalState = serde_json::from_value(view(&draft).await).unwrap();
    let goal = goal.goal.as_mut().unwrap();
    goal.reserve().unwrap();
    let expected = goal.reservation.as_ref().unwrap().input(&goal.id);
    let mut context = rsi_agent_composition_protocol::SessionCommandContext {
        request_id: goal
            .reservation
            .as_ref()
            .unwrap()
            .request_id
            .clone()
            .unwrap(),
        header: Arc::new(draft.header().clone()),
        revision: CommandRevision::Durable { control_seq: 1 },
        domains: draft
            .baseline()
            .initial_states()
            .into_iter()
            .map(|snapshot| DomainStateView {
                revision: DomainRevision::new(0),
                snapshot,
            })
            .collect(),
        continuation_input: Some(expected.clone()),
    };
    let command = draft
        .composition()
        .contributions()
        .entries()
        .iter()
        .find_map(|entry| match entry.kind() {
            ContributionKind::Command(command)
                if entry.id().as_str() == rsi_agent_goal::GOAL_RESERVE =>
            {
                Some(command.clone())
            }
            _ => None,
        })
        .unwrap();
    let arguments = CommandArguments::new(serde_json::json!({"id":"goal-task"})).unwrap();
    assert!(
        command
            .callback()
            .execute(&context, &arguments, CancellationToken::new())
            .await
            .is_ok()
    );
    let original_request = context.request_id.clone();
    context.request_id = DomainRequestId::new("different-reservation-receipt").unwrap();
    assert!(
        command
            .callback()
            .execute(&context, &arguments, CancellationToken::new())
            .await
            .is_err()
    );
    context.request_id = original_request;
    for field in ["owner", "round", "message_id", "text", "absent"] {
        let mut input = expected.clone();
        match field {
            "owner" => input.owner = DomainRequestId::new("other").unwrap(),
            "round" => input.round += 1,
            "message_id" => {
                input.message_id = rsi_agent_session_protocol::MessageId::new("other").unwrap();
            }
            "text" => input.text.push_str(" different"),
            _ => {}
        }
        context.continuation_input = (field != "absent").then_some(input);
        assert!(
            command
                .callback()
                .execute(&context, &arguments, CancellationToken::new())
                .await
                .is_err(),
            "{field}"
        );
    }
    drop(draft);
    fixture.stop().await;
}

#[derive(Debug)]
struct ForbiddenSandbox;
#[async_trait::async_trait]
impl rsi_sandbox::Sandbox for ForbiddenSandbox {
    async fn workspace_read(
        &self,
        _: rsi_sandbox::WorkspaceReadRequest,
    ) -> rsi_sandbox::Result<rsi_sandbox::WorkspaceReadScope> {
        panic!("Goal reporting must not read Workspace state");
    }
    async fn confine(
        &self,
        _: rsi_sandbox::ProcessRequest,
    ) -> rsi_sandbox::Result<rsi_sandbox::ConfinedProcess> {
        panic!("Goal reporting must not start external effects");
    }
}

fn tool_start(fixture: &Fixture) -> rsi_tools_protocol::ToolStart {
    rsi_tools_protocol::ToolStart {
        cancellation: CancellationToken::new(),
        policy: rsi_tools_protocol::ToolExecutionPolicy {
            mode: SandboxMode::ReadOnly,
            cwd: fixture.temp.path().into(),
            workspace: fixture.temp.path().into(),
        },
        sandbox: Arc::new(ForbiddenSandbox),
        job_scope: None,
        extensions: rsi_tools_protocol::ToolExecutionExtensions::default(),
    }
}

#[derive(Debug)]
struct Facts(Vec<Arc<rsi_agent_session_protocol::SessionFact>>);
#[async_trait::async_trait]
impl rsi_agent_composition_protocol::ContributionFactReader for Facts {
    async fn read(
        &self,
        after_seq: u64,
        limit: usize,
    ) -> rsi_agent_composition_protocol::ContributionResult<
        rsi_agent_composition_protocol::ContributionFactPage,
    > {
        let facts = self
            .0
            .iter()
            .filter(|fact| fact.seq() > after_seq)
            .take(limit)
            .cloned()
            .collect::<Vec<_>>();
        let through_seq = facts.last().map_or(after_seq, |fact| fact.seq());
        Ok(rsi_agent_composition_protocol::ContributionFactPage { facts, through_seq })
    }
}

#[tokio::test]
#[allow(clippy::too_many_lines)] // Keep the compared source Fact tuples and each forged field visible together.
async fn report_contribution_authenticates_tool_name_identity_arguments_and_source_turn() {
    use rsi_agent_composition_protocol::{ContributionContext, ContributionHorizon};
    use rsi_agent_session_protocol::{
        ActivationId, EffectId, SessionFact, SessionFactBody, StepId, TurnId,
    };
    use rsi_tools_protocol::{ToolContent, ToolResult, ToolResultIdentity};
    let fixture = Fixture::new().await;
    let mut draft = fixture.draft().await;
    create(&mut draft).await;
    let domains = draft
        .baseline()
        .initial_states()
        .into_iter()
        .map(|snapshot| DomainStateView {
            revision: DomainRevision::new(1),
            snapshot,
        })
        .collect::<Vec<_>>();
    let state: rsi_agent_goal::GoalState =
        serde_json::from_value(domains[0].snapshot.state().value().clone()).unwrap();
    let message = state.goal.unwrap().reservation.unwrap().message_id;
    let turn = TurnId::new("source-turn").unwrap();
    let step = StepId::new("source-step").unwrap();
    let identity = ToolResultIdentity::new("tools", "invocation", "call", "a".repeat(64)).unwrap();
    let args = serde_json::json!({"goal_id":"goal-task","kind":"complete","evidence":"Independent tests passed"});
    let result = ToolResult::new(
        serde_json::json!({"goal_report":{"version":1,"report":args}}),
        vec![ToolContent::Text {
            text: "Goal report claim received".into(),
        }],
        false,
    )
    .unwrap();
    let effect = EffectId::new("report-effect").unwrap();
    let post = draft
        .composition()
        .contributions()
        .entries()
        .iter()
        .find_map(|entry| match entry.kind() {
            ContributionKind::PostTool(callback) => Some(callback.clone()),
            _ => None,
        })
        .unwrap();
    let policy = draft
        .composition()
        .contributions()
        .entries()
        .iter()
        .find_map(|entry| match entry.kind() {
            ContributionKind::ToolPolicy(callback) => Some(callback.clone()),
            _ => None,
        })
        .unwrap();
    for fault in [
        "valid",
        "wrong-name",
        "wrong-identity",
        "wrong-args",
        "wrong-message",
        "wrong-turn",
    ] {
        let accepted = Arc::new(
            SessionFact::new(
                1,
                1,
                SessionFactBody::MessageTurnAccepted {
                    turn_id: turn.clone(),
                    activation_id: ActivationId::new("activation").unwrap(),
                    message_ids: vec![if fault == "wrong-message" {
                        rsi_agent_session_protocol::MessageId::new("other").unwrap()
                    } else {
                        message.clone()
                    }],
                    model: None,
                    sandbox: SandboxMode::ReadOnly,
                    require_approval: false,
                },
            )
            .unwrap(),
        );
        let intent = Arc::new(SessionFact::new(2, 1, SessionFactBody::ToolIntent { turn_id: turn.clone(), effect_id: effect.clone(), identity: if fault == "wrong-identity" { ToolResultIdentity::new("other", "invocation", "call", "a".repeat(64)).unwrap() } else { identity.clone() }, name: if fault == "wrong-name" { "bash" } else { "report_goal" }.into(), arguments: if fault == "wrong-args" { serde_json::json!({"goal_id":"goal-task","kind":"blocked","evidence":"different"}) } else { args.clone() }, approval: None, parallel_safe: false }).unwrap());
        let settled = Arc::new(
            SessionFact::new(
                3,
                1,
                SessionFactBody::ToolResult {
                    turn_id: if fault == "wrong-turn" {
                        TurnId::new("other-turn").unwrap()
                    } else {
                        turn.clone()
                    },
                    effect_id: effect.clone(),
                    identity: identity.clone(),
                    result: result.clone(),
                },
            )
            .unwrap(),
        );
        let context = ContributionContext {
            header: Arc::new(draft.header().clone()),
            turn_id: turn.clone(),
            accepted_fact_seq: 1,
            step_id: step.clone(),
            horizon: ContributionHorizon {
                fact_seq: 3,
                control_seq: 2,
            },
            domains: domains.clone().into(),
            facts: Arc::new(Facts(vec![accepted, intent, settled.clone()])),
        };
        let output = post
            .contribute(&context, &[settled], CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(
            output.domains.len(),
            usize::from(fault == "valid"),
            "accepted {fault}"
        );
        if let Some(proposal) = output.domains.first() {
            assert_eq!(
                proposal.snapshot().state().value()["goal"]["phase"],
                "active"
            );
            assert_eq!(
                proposal.snapshot().state().value()["goal"]["report"]["source_turn"],
                "source-turn"
            );
        }
        let decision = policy
            .decide(
                &context,
                &rsi_agent_composition_protocol::ToolPolicyRequest {
                    identity: &identity,
                    name: "report_goal",
                    arguments: &args,
                    sandbox: SandboxMode::ReadOnly,
                    require_approval: false,
                },
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(
            matches!(
                decision,
                rsi_agent_composition_protocol::ToolPolicyDecision::Deny { .. }
            ),
            fault == "wrong-message"
        );
    }
    drop((draft, post, policy));
    fixture.stop().await;
}
