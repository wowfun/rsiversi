use rsi_agent_composition::{
    AgentCompositionFactory, AgentContributionCatalog, AgentGenerationRootFactory,
};
use rsi_agent_composition_protocol::{
    AgentComposition, AgentCompositionContract, AgentSessionDraft, ContributionKind,
    DraftCommandPreparation, SessionProjectionAdapter, SessionProjectionContext,
    ToolPolicyDecision, ToolPolicyRequest,
};
use rsi_agent_kernel::AgentKernel;
use rsi_agent_plan_policy::PlanPolicyFactory;
use rsi_agent_presets::{
    AgentPresetCatalog, AgentPresetCatalogConfig, AgentPresetProfileCompiler, AgentPresetRoot,
    AgentPresetTrust,
};
use rsi_agent_session_protocol::{
    AgentPresetId, CommandArguments, DomainRequestId, DomainRevision, DomainStateView,
    FrozenAgentSettings, ProjectionCursor, SessionCommandInvocation, SessionHeader, SessionId,
    TurnId,
};
use rsi_agent_store_protocol::SessionStore;
use rsi_agent_testkit::MemoryStore;
use rsi_agent_turn_protocol::{
    SessionCommands, SessionProjections, SubmitSession, SubmitTurn, TurnExecution, TurnService,
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
        std::fs::write(preset.join("agent.profile.toml"), "format = 1\n[[steps]]\nkind = \"plugin\"\nid = \"context\"\nplugin = \"fixture.context\"\n[[steps]]\nkind = \"plugin\"\nid = \"plan\"\nplugin = \"fixture.plan\"\n").unwrap();
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
                ["fixture.context", "fixture.plan"],
            ),
        )
        .unwrap();
        let contributions = AgentContributionCatalog::new([
            linked(
                "fixture.context",
                rsi_agent_context::DefaultContextBuilderFactory,
            ),
            linked("fixture.plan", PlanPolicyFactory),
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
            SessionId::new("plan-fixture").unwrap(),
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

fn invocation(draft: &AgentSessionDraft, id: &str, argument: &str) -> SessionCommandInvocation {
    let descriptors = draft.command_descriptors();
    assert_eq!(descriptors.len(), 1);
    assert_eq!(descriptors[0].name(), "plan");
    assert!(descriptors[0].draft_safe());
    SessionCommandInvocation {
        command: descriptors[0].id().clone(),
        request_id: DomainRequestId::new(id).unwrap(),
        expected_revision: draft.revision(),
        arguments: CommandArguments::new(argument.into()).unwrap(),
    }
}
async fn command(draft: &mut AgentSessionDraft, id: &str, argument: &str) {
    let DraftCommandPreparation::Run(prepared) = draft
        .prepare_command(invocation(draft, id, argument))
        .unwrap()
    else {
        panic!("new command")
    };
    let mutation = prepared.execute(CancellationToken::new()).await.unwrap();
    draft.apply_command(mutation).unwrap();
}
async fn view(draft: &AgentSessionDraft) -> serde_json::Value {
    let rsi_agent_session_protocol::CommandRevision::Draft { revision } = draft.revision() else {
        panic!("draft revision")
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
async fn ordinary_factory_owns_disabled_defaults_commands_projection_and_preset_reset() {
    let fixture = Fixture::new().await;
    let mut draft = fixture.draft().await;
    assert_eq!(draft.composition().contributions().entries().len(), 4);
    assert_eq!(
        view(&draft).await,
        serde_json::json!({"enabled":false,"allow_tools":["ask_user","directory_list","file_read","output_read"]})
    );
    for (id, argument, expected) in [
        ("on", "on", true),
        ("off", "off", false),
        ("toggle", "toggle", true),
        ("empty", "", false),
    ] {
        command(&mut draft, id, argument).await;
        assert_eq!(view(&draft).await["enabled"], expected);
    }
    let before = draft.revision();
    let DraftCommandPreparation::Run(prepared) = draft
        .prepare_command(invocation(&draft, "bad", "enable"))
        .unwrap()
    else {
        panic!("new command")
    };
    assert!(prepared.execute(CancellationToken::new()).await.is_err());
    assert_eq!(draft.revision(), before);
    command(&mut draft, "on-again", "on").await;
    let prepared = draft
        .prepare_preset_selection(AgentPresetId::new("fixture").unwrap())
        .await
        .unwrap();
    draft.apply_preset_selection(prepared).unwrap();
    assert_eq!(view(&draft).await["enabled"], false);
    drop(draft);
    fixture.stop().await;
}

#[tokio::test]
async fn actual_draft_state_enters_kernel_and_policy_only_adds_constraints() {
    let fixture = Fixture::new().await;
    let mut draft = fixture.draft().await;
    command(&mut draft, "enable", "on").await;
    let pin = draft.composition().clone();
    let store = Arc::new(MemoryStore::new());
    let kernel = AgentKernel::recover(store.clone(), fixture.composition.clone())
        .await
        .unwrap();
    let workers = kernel.start_workers();
    kernel
        .submit(SubmitTurn {
            session: SubmitSession::Fresh(draft.into_fresh()),
            turn_id: TurnId::new("first").unwrap(),
            text: "plan".into(),
            model: None,
            sandbox: None,
        })
        .await
        .unwrap();
    let executor = kernel.register("fixture".into()).unwrap();
    let claim = kernel
        .claim("fixture", CancellationToken::new())
        .await
        .unwrap()
        .unwrap();
    let context = kernel
        .contribution_context(&claim, CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(context.domains[0].snapshot.state().value(), &true);
    assert_enabled_plan(&pin, &context).await;
    let snapshot = kernel
        .projection_snapshot(fixture.header().session_id())
        .await
        .unwrap();
    assert_eq!(
        snapshot.entries()[0].view().unwrap().value()["enabled"],
        true
    );
    let prepared = kernel
        .prepare_resume(fixture.header().session_id())
        .await
        .unwrap();
    let commands = kernel
        .list(
            kernel
                .prepare_resume(fixture.header().session_id())
                .await
                .unwrap(),
        )
        .await
        .unwrap();
    kernel
        .execute(
            prepared,
            SessionCommandInvocation {
                command: commands.commands()[0].id().clone(),
                request_id: DomainRequestId::new("disable").unwrap(),
                expected_revision: commands.revision(),
                arguments: CommandArguments::new("off".into()).unwrap(),
            },
        )
        .await
        .unwrap();
    let current = kernel
        .contribution_context(&claim, CancellationToken::new())
        .await
        .unwrap();
    let identity =
        rsi_tools_protocol::ToolResultIdentity::new("owner", "invocation", "call", "a".repeat(64))
            .unwrap();
    let policy = pinned_policy(&pin);
    let request = ToolPolicyRequest {
        identity: &identity,
        name: "bash",
        arguments: &serde_json::Value::Null,
        sandbox: SandboxMode::ReadOnly,
        require_approval: true,
    };
    assert_eq!(
        policy
            .decide(&current, &request, CancellationToken::new())
            .await
            .unwrap(),
        ToolPolicyDecision::Abstain
    );
    assert_eq!(
        store
            .read_domain_states(fixture.header().session_id(), None)
            .await
            .unwrap()
            .states[0]
            .snapshot
            .state()
            .value(),
        &false
    );
    drop((executor, pin, context, current));
    kernel.shutdown(workers).await.unwrap();
    fixture.stop().await;
}

fn pinned_policy(
    pin: &rsi_agent_composition_protocol::AgentCompositionPin,
) -> &Arc<dyn rsi_agent_composition_protocol::ToolPolicy> {
    pin.contributions()
        .entries()
        .iter()
        .find_map(|entry| {
            if let ContributionKind::ToolPolicy(policy) = entry.kind() {
                Some(policy)
            } else {
                None
            }
        })
        .unwrap()
}

async fn assert_enabled_plan(
    pin: &rsi_agent_composition_protocol::AgentCompositionPin,
    context: &rsi_agent_composition_protocol::ContributionContext,
) {
    let identity =
        rsi_tools_protocol::ToolResultIdentity::new("owner", "invocation", "call", "a".repeat(64))
            .unwrap();
    let policy = pinned_policy(pin);
    for (name, denied) in [
        ("ask_user", false),
        ("output_read", false),
        ("directory_list", false),
        ("file_read", false),
        ("file_read_more", true),
        ("bash", true),
        ("output_read_more", true),
        ("apply_patch", true),
    ] {
        let request = ToolPolicyRequest {
            identity: &identity,
            name,
            arguments: &serde_json::Value::Null,
            sandbox: SandboxMode::ReadOnly,
            require_approval: true,
        };
        let result = policy
            .decide(context, &request, CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(matches!(result, ToolPolicyDecision::Deny { .. }), denied);
        if !denied {
            assert_eq!(result, ToolPolicyDecision::Abstain);
        }
    }
    let context_unit = pin
        .contributions()
        .entries()
        .iter()
        .find_map(|entry| {
            if let ContributionKind::Context(unit) = entry.kind() {
                Some(unit)
            } else {
                None
            }
        })
        .unwrap();
    let output = context_unit
        .contribute(context, CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(output.inputs.len(), 1);
    assert!(output.domains.is_empty());
}

#[test]
fn configuration_rejects_duplicates_unknown_fields_invalid_names_and_overflow() {
    for value in [
        serde_json::json!({"allow_tools":["bash","bash"]}),
        serde_json::json!({"allow_tools":["has space"]}),
        serde_json::json!({"allow_tools":[],"enabled":true}),
        serde_json::json!({"allow_tools":(0..65).map(|n| format!("tool{n}")).collect::<Vec<_>>()}),
    ] {
        assert!(PlanPolicyFactory.prepare(&value).is_err());
    }
    assert!(PlanPolicyFactory.prepare(&ConfigValue::Null).is_ok());
    assert!(
        PlanPolicyFactory
            .prepare(&serde_json::json!({"allow_tools":[]}))
            .is_ok()
    );
}
