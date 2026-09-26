use super::*;
use rsi_agent_session_protocol::ToolOrigin;
use rsi_tools_protocol::ToolProgramRole;

#[derive(Debug)]
struct ProgramTools;
#[async_trait]
impl ToolRuntime for ProgramTools {
    fn program_role(&self, name: &str) -> Option<rsi_tools_protocol::ToolProgramRole> {
        self.definition(name)
            .map(|definition| definition.program_role())
    }
    fn program_roles(
        &self,
    ) -> std::collections::BTreeMap<String, rsi_tools_protocol::ToolProgramRole> {
        self.definitions()
            .into_iter()
            .filter(|definition| {
                definition.program_role() != rsi_tools_protocol::ToolProgramRole::Unavailable
            })
            .map(|definition| (definition.name().to_owned(), definition.program_role()))
            .collect()
    }
    fn definition(&self, name: &str) -> Option<rsi_tools_protocol::ToolDefinition> {
        self.definitions()
            .into_iter()
            .find(|definition| definition.name() == name)
    }
    fn definitions(&self) -> Vec<ToolDefinition> {
        [
            ("run_code", ToolProgramRole::Coordinator),
            ("fixture_workflow", ToolProgramRole::Workflow),
            ("run_workflow", ToolProgramRole::Unavailable),
            ("fixture_control", ToolProgramRole::Unavailable),
            ("file_read", ToolProgramRole::Callable),
        ]
        .into_iter()
        .map(|(name, role)| {
            ToolDefinition::new(name, "fixture", serde_json::json!({"type":"object"}))
                .unwrap()
                .with_program_role(role)
        })
        .collect()
    }
    fn prepare(
        &self,
        id: &str,
        call: ToolCall,
    ) -> rsi_tools_protocol::Result<Box<dyn PreparedToolCall>> {
        SourceOnlyTools.prepare(id, call)
    }
    fn query(
        &self,
        identity: &ToolResultIdentity,
    ) -> rsi_tools_protocol::Result<RetainedToolResult> {
        SourceOnlyTools.query(identity)
    }
    async fn wait(
        &self,
        identity: &ToolResultIdentity,
        cancellation: CancellationToken,
    ) -> rsi_tools_protocol::Result<RetainedToolResult> {
        SourceOnlyTools.wait(identity, cancellation).await
    }
    fn commit(&self, identity: &ToolResultIdentity) -> rsi_tools_protocol::Result<()> {
        SourceOnlyTools.commit(identity)
    }
}
#[derive(Debug)]
struct ProgramComposition(AgentCompositionPin);
#[async_trait]
impl AgentComposition for ProgramComposition {
    async fn default_preset_id(&self) -> rsi_agent_composition_protocol::Result<AgentPresetId> {
        Ok(self.0.preset_id().clone())
    }
    async fn pin(
        &self,
        _: &AgentPresetId,
        _: Option<&rsi_agent_composition_protocol::AgentGenerationSeed>,
    ) -> rsi_agent_composition_protocol::Result<AgentCompositionPin> {
        Ok(self.0.clone())
    }
}
fn identity(id: &str) -> ToolResultIdentity {
    ToolResultIdentity::new("program-test", id, id, "a".repeat(64)).unwrap()
}
fn intent(
    turn_id: &TurnId,
    id: &str,
    origin: ToolOrigin,
    name: &str,
    role: ToolProgramRole,
) -> SessionFactBody {
    SessionFactBody::ToolIntent {
        turn_id: turn_id.clone(),
        effect_id: EffectId::new(id).unwrap(),
        origin,
        program_role: role,
        identity: identity(id),
        name: name.into(),
        arguments: serde_json::json!({}),
        approval: None,
        parallel_safe: false,
    }
}
fn started(turn_id: &TurnId, id: &str) -> SessionFactBody {
    SessionFactBody::ToolStarted {
        turn_id: turn_id.clone(),
        effect_id: EffectId::new(id).unwrap(),
        identity: identity(id),
    }
}
fn result(turn_id: &TurnId, id: &str, text: &str) -> SessionFactBody {
    SessionFactBody::ToolResult {
        turn_id: turn_id.clone(),
        effect_id: EffectId::new(id).unwrap(),
        identity: identity(id),
        result: rsi_tools_protocol::ToolResult::new(
            serde_json::json!({"text":text}),
            vec![rsi_tools_protocol::ToolContent::Text { text: text.into() }],
            false,
        )
        .unwrap(),
        conclusion: None,
    }
}

#[tokio::test]
#[allow(
    clippy::too_many_lines,
    reason = "One exact claim proves source admission, nested lifecycle, checkpoint and cold recovery together."
)]
async fn nested_program_calls_bind_frozen_catalog_parent_and_ordinals_and_stay_out_of_context() {
    let store = Arc::new(MemoryStore::new());
    let composition = Arc::new(ProgramComposition(
        AgentCompositionPin::new(
            AgentPresetId::new("test-agent").unwrap(),
            "a".repeat(64),
            Arc::new(ProgramTools),
            Arc::new(rsi_agent_context::DefaultContextBuilder::default()),
            rsi_agent_composition_protocol::DomainCatalog::default(),
            rsi_agent_composition_protocol::ContributionCatalog::default(),
            Arc::new(()),
        )
        .unwrap(),
    ));
    let kernel =
        AgentKernel::recover_with_clock(store.clone(), composition.clone(), Arc::new(FixedClock))
            .await
            .unwrap();
    let workers = kernel.start_workers();
    kernel
        .submit(SubmitTurn {
            reasoning_effort: None,
            session: SubmitSession::Fresh(
                PreparedFreshSession::new(header("program"), composition.0.clone()).unwrap(),
            ),
            turn_id: TurnId::new("program-turn").unwrap(),
            text: "work".into(),
            model: None,
            sandbox: None,
        })
        .await
        .unwrap();
    let executor = kernel.register("program".into()).unwrap();
    let claim = kernel
        .claim("program", CancellationToken::new())
        .await
        .unwrap()
        .unwrap();
    kernel.composition(&claim).unwrap();
    flush_bodies(
        &kernel,
        &claim,
        vec![SessionFactBody::StepStarted {
            turn_id: claim.turn_id().clone(),
            step_id: rsi_agent_session_protocol::StepId::new("program-step").unwrap(),
        }],
    )
    .await;
    publish_model_source(&kernel, &claim, "outer", "run_code", &snapshot()).await;
    let turn = claim.turn_id();
    let outer = intent(
        turn,
        "outer",
        ToolOrigin::Model {
            effect_id: EffectId::new("source-model").unwrap(),
        },
        "run_code",
        ToolProgramRole::Coordinator,
    );
    let mut forged = outer.clone();
    if let SessionFactBody::ToolIntent { name, .. } = &mut forged {
        *name = "unregistered".into();
    }
    assert!(kernel.publish(&claim, vec![forged]).await.is_err());
    flush_bodies(&kernel, &claim, vec![outer]).await;
    let nested = |id: &str, ordinal| {
        intent(
            turn,
            id,
            ToolOrigin::Program {
                parent_effect_id: EffectId::new("outer").unwrap(),
                ordinal,
            },
            "file_read",
            ToolProgramRole::Callable,
        )
    };
    assert!(
        kernel
            .publish(&claim, vec![nested("one", 1)])
            .await
            .is_err(),
        "parent not started"
    );
    assert!(
        kernel
            .program_policy_context(
                &claim,
                &EffectId::new("outer").unwrap(),
                CancellationToken::new()
            )
            .await
            .is_err()
    );
    flush_bodies(&kernel, &claim, vec![started(turn, "outer")]).await;
    let creator = kernel
        .tool_caller(&claim, &EffectId::new("outer").unwrap())
        .unwrap();
    assert!(
        kernel
            .prepare_program(rsi_agent_turn_protocol::PrepareProgram {
                caller: creator,
                cancellation: CancellationToken::new(),
                script: "return null".into(),
                fork_turns: rsi_agent_session_protocol::ForkTurnSelection::None,
                guard: None,
                continuation_domains: vec![],
            })
            .await
            .is_err(),
        "foreground Coordinator cannot acquire detached Workflow authority"
    );

    assert!(
        kernel
            .contribution_context(&claim, CancellationToken::new())
            .await
            .is_err()
    );
    assert!(
        kernel
            .program_policy_context(
                &claim,
                &EffectId::new("foreign").unwrap(),
                CancellationToken::new()
            )
            .await
            .is_err()
    );
    kernel
        .program_policy_context(
            &claim,
            &EffectId::new("outer").unwrap(),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert!(
        kernel
            .publish(&claim, vec![nested("skip", 2)])
            .await
            .is_err(),
        "ordinal cannot skip"
    );
    let mut recursive = nested("recursive", 1);
    if let SessionFactBody::ToolIntent {
        name, program_role, ..
    } = &mut recursive
    {
        *name = "run_code".into();
        *program_role = ToolProgramRole::Coordinator;
    }
    assert!(kernel.publish(&claim, vec![recursive]).await.is_err());
    flush_bodies(&kernel, &claim, vec![nested("one", 1)]).await;
    assert!(
        kernel
            .program_policy_context(
                &claim,
                &EffectId::new("outer").unwrap(),
                CancellationToken::new()
            )
            .await
            .is_err()
    );
    assert!(
        kernel
            .publish(&claim, vec![nested("overlap", 2)])
            .await
            .is_err(),
        "exclusive nested call is a barrier"
    );
    assert!(
        kernel
            .publish(&claim, vec![result(turn, "outer", "early")])
            .await
            .is_err(),
        "parent cannot abandon its nested effect"
    );
    flush_bodies(&kernel, &claim, vec![started(turn, "one")]).await;
    let prefix = store
        .read_facts(claim.session_id(), 0, 128)
        .await
        .unwrap()
        .facts;
    let limits = rsi_agent_context::ContextLimits::default();
    let mut fold =
        rsi_agent_context::ContextFold::with_limits(claim.header().clone(), limits).unwrap();
    fold.apply(&prefix).unwrap();
    let checkpoint = fold.checkpoint_bytes().unwrap();
    let mut restored = rsi_agent_context::ContextFold::from_checkpoint(
        claim.header().clone(),
        limits,
        &checkpoint,
    )
    .unwrap();
    flush_bodies(
        &kernel,
        &claim,
        vec![result(turn, "one", "INTERNAL_RESULT_ONLY")],
    )
    .await;
    assert!(
        kernel
            .publish(&claim, vec![nested("reused", 1)])
            .await
            .is_err()
    );
    flush_bodies(&kernel, &claim, vec![nested("two", 2)]).await;
    flush_bodies(&kernel, &claim, vec![started(turn, "two")]).await;
    flush_bodies(
        &kernel,
        &claim,
        vec![result(turn, "two", "SECOND_INTERNAL_RESULT")],
    )
    .await;
    flush_bodies(
        &kernel,
        &claim,
        vec![result(turn, "outer", "CURATED_FINAL")],
    )
    .await;
    assert!(
        kernel
            .publish(&claim, vec![nested("retired", 3)])
            .await
            .is_err()
    );
    assert!(
        kernel
            .program_policy_context(
                &claim,
                &EffectId::new("outer").unwrap(),
                CancellationToken::new()
            )
            .await
            .is_err()
    );
    kernel
        .finish_turn(&claim, &TurnOutcome::Completed)
        .await
        .unwrap();
    let suffix = store
        .read_facts(claim.session_id(), prefix.last().unwrap().seq(), 128)
        .await
        .unwrap()
        .facts;
    fold.apply(&suffix).unwrap();
    restored.apply(&suffix).unwrap();
    let visible = serde_json::to_string(&fold.project(limits).unwrap().messages).unwrap();
    assert!(visible.contains("CURATED_FINAL"));
    assert!(!visible.contains("INTERNAL_RESULT"));
    assert!(!visible.contains("file_read"));
    assert_eq!(
        fold.project(limits).unwrap(),
        restored.project(limits).unwrap()
    );
    drop(executor);
    kernel.shutdown(workers).await.unwrap();
    let cold = AgentKernel::recover_with_clock(store, composition, Arc::new(FixedClock))
        .await
        .unwrap();
    let workers = cold.start_workers();
    cold.shutdown(workers).await.unwrap();
}

#[path = "program/workflow.rs"]
mod workflow;
