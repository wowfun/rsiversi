use async_trait::async_trait;
use rsi_agent_composition::{
    AgentCompositionFactory, AgentContributionCatalog, AgentGenerationRootFactory,
};
use rsi_agent_composition_protocol::{
    AgentComposition, AgentCompositionContract, ContributionBatch, ContributionKind,
    PreparedFreshSession,
};
use rsi_agent_kernel::AgentKernel;
use rsi_agent_presets::{
    AgentPresetCatalog, AgentPresetCatalogConfig, AgentPresetProfileCompiler, AgentPresetRoot,
    AgentPresetTrust, COMPOSITION_FILE,
};
use rsi_agent_session_protocol::{
    AgentMessage, AgentMessageContent, AgentMessageSource, AgentPresetId, DomainRequestId,
    FrozenAgentSettings, InputMessageSource, MessageId, MessageOptions, SessionFactBody,
    SessionHeader, SessionId, TurnId, TurnOutcome,
};
use rsi_agent_store_protocol::SessionStore;
use rsi_agent_store_sqlite::SqliteStore;
use rsi_agent_turn_protocol::{
    DomainMutation, SubmitSession, SubmitTurn, TurnClaim, TurnExecution, TurnService,
};
use rsi_agent_workspace_context::{
    WorkspaceContext, WorkspaceContextContract, WorkspaceContextError, WorkspaceContextSnapshot,
    WorkspaceContributorFactory, WorkspaceSkillInvocation, WorkspaceSkillRequests,
};
use rsi_ai_protocol::ModelRef;
use rsi_meta::{
    ActivationPlan, ConfigValue, PluginFactory, PreparedActivation, ResolvedFactory, Runtime,
    UpdateMode,
};
use rsi_meta_profile::{ProfileCompiler, ProfileEnvironment, ProfileLimits};
use rsi_meta_scope::ScopeRoot;
use rsi_sandbox::SandboxMode;
use std::{
    collections::BTreeMap,
    fs,
    path::PathBuf,
    sync::{Arc, Mutex},
};
use tokio_util::sync::CancellationToken;

#[derive(Debug)]
struct Source {
    snapshot: Mutex<WorkspaceContextSnapshot>,
    requests: Mutex<Vec<Vec<String>>>,
}
#[async_trait]
impl WorkspaceContext for Source {
    async fn snapshot(
        &self,
        _: &SessionHeader,
        requests: &WorkspaceSkillRequests,
    ) -> Result<WorkspaceContextSnapshot, WorkspaceContextError> {
        self.requests
            .lock()
            .unwrap()
            .push(requests.names().to_vec());
        let mut snapshot = self.snapshot.lock().unwrap().clone();
        snapshot.invocations = requests
            .names()
            .iter()
            .map(|name| WorkspaceSkillInvocation {
                name: name.clone(),
                source: "fixture/SKILL.md".into(),
                text: format!("Instructions for {name}"),
            })
            .collect();
        Ok(snapshot)
    }
}
#[derive(Debug)]
struct SourceFactory(Arc<Source>);
#[async_trait]
impl PluginFactory for SourceFactory {
    fn prepare(&self, _: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        Ok(PreparedActivation::new(ConfigValue::Null))
    }
    async fn activate(&self, plan: ActivationPlan) -> rsi_meta::Result<()> {
        let supply = plan
            .context()
            .provide_local::<WorkspaceContextContract>(self.0.clone())?;
        plan.defer(
            "withdraw source",
            Box::new(move || {
                Box::pin(async move {
                    drop(supply);
                    Ok(())
                })
            }),
        )
    }
}

struct Fixture {
    temp: tempfile::TempDir,
    profile: PathBuf,
    runtime: Runtime,
    service: Arc<dyn AgentComposition>,
    source: Arc<Source>,
}
fn snapshot(instructions: bool) -> WorkspaceContextSnapshot {
    WorkspaceContextSnapshot {
        complete: true,
        instructions_sha256: if instructions { "a" } else { "b" }.repeat(64),
        instructions: instructions.then(|| "Workspace instructions".into()),
        skill_catalog_sha256: "c".repeat(64),
        skill_catalog: None,
        invocations: vec![],
    }
}
fn source_program(context_id: &str) -> String {
    format!(
        "format = 1\n[[steps]]\nkind = \"plugin\"\nid = \"{context_id}\"\nplugin = \"fixture.context\"\n[[steps]]\nkind = \"plugin\"\nid = \"workspace\"\nplugin = \"fixture.workspace\"\n"
    )
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
    async fn new(initial: WorkspaceContextSnapshot) -> Self {
        let temp = tempfile::tempdir().unwrap();
        let preset_root = temp.path().join("presets");
        let directory = preset_root.join("test-agent");
        fs::create_dir_all(&directory).unwrap();
        let profile = directory.join(COMPOSITION_FILE);
        fs::write(&profile, source_program("context")).unwrap();
        for name in ["config", "state", "cache"] {
            fs::create_dir_all(temp.path().join(name)).unwrap();
        }
        let environment = ProfileEnvironment::new(
            temp.path().join("config"),
            temp.path().join("state"),
            temp.path().join("cache"),
            "test",
            BTreeMap::new(),
        )
        .unwrap();
        let compiler = AgentPresetProfileCompiler::new(
            ProfileCompiler::new(environment, ProfileLimits::default()),
            ["fixture.context", "fixture.workspace"],
        );
        let presets = AgentPresetCatalog::new(
            AgentPresetCatalogConfig::new(AgentPresetId::new("test-agent").unwrap())
                .with_configured_root(
                    AgentPresetRoot::new(preset_root, AgentPresetTrust::User).unwrap(),
                ),
            compiler,
        )
        .unwrap();
        let contributions = AgentContributionCatalog::new([
            linked(
                "fixture.context",
                rsi_agent_context::DefaultContextBuilderFactory,
            ),
            linked("fixture.workspace", WorkspaceContributorFactory),
        ])
        .unwrap();
        let runtime = Runtime::default();
        let source = Arc::new(Source {
            snapshot: Mutex::new(initial),
            requests: Mutex::new(vec![]),
        });
        for factory in [
            linked("fixture.source", SourceFactory(source.clone())),
            linked("fixture.tools", rsi_tools::ToolsFactory),
            linked("fixture.root", AgentGenerationRootFactory),
            linked(
                "fixture.composition",
                AgentCompositionFactory::new(presets, contributions, ScopeRoot::new(32).unwrap()),
            ),
        ] {
            runtime
                .root()
                .apply(factory, ConfigValue::Null)
                .await
                .unwrap();
        }
        let service = runtime
            .root()
            .lookup_local::<AgentCompositionContract>()
            .unwrap();
        Self {
            temp,
            profile,
            runtime,
            service,
            source,
        }
    }
    fn header(&self, id: &str) -> SessionHeader {
        SessionHeader::new(
            SessionId::new(id).unwrap(),
            1,
            self.temp.path().to_str().unwrap(),
            AgentPresetId::new("test-agent").unwrap(),
            FrozenAgentSettings::new(
                "default",
                "system",
                ModelRef::new("deployment", "model").unwrap(),
                SandboxMode::WorkspaceWrite,
                false,
            )
            .unwrap(),
        )
        .unwrap()
    }
    async fn submit(
        &self,
        kernel: &AgentKernel,
        id: &str,
        turn: &str,
        text: &str,
        existing: bool,
    ) -> TurnClaim {
        let session = if existing {
            SubmitSession::Resume(
                kernel
                    .prepare_resume(&SessionId::new(id).unwrap())
                    .await
                    .unwrap(),
            )
        } else {
            SubmitSession::Fresh(
                PreparedFreshSession::new(
                    self.header(id),
                    self.service
                        .pin(&AgentPresetId::new("test-agent").unwrap())
                        .await
                        .unwrap(),
                )
                .unwrap(),
            )
        };
        kernel
            .submit(SubmitTurn {
                session,
                turn_id: TurnId::new(turn).unwrap(),
                text: text.into(),
                model: None,
                sandbox: None,
            })
            .await
            .unwrap();
        kernel
            .claim("fixture", CancellationToken::new())
            .await
            .unwrap()
            .unwrap()
    }
    async fn close(self) {
        drop(self.service);
        assert!(self.runtime.shutdown().await.is_clean());
    }
}

async fn enter(kernel: &AgentKernel, claim: &TurnClaim, request: &str) -> usize {
    let pin = kernel.composition(claim).unwrap();
    let token = CancellationToken::new();
    let context = kernel
        .contribution_context(claim, token.clone())
        .await
        .unwrap();
    let mut batch = ContributionBatch::default();
    for entry in pin.contributions().entries() {
        let ContributionKind::Context(callback) = entry.kind() else {
            panic!("workspace callback");
        };
        let output = callback.contribute(&context, token.clone()).await.unwrap();
        batch
            .append(entry.id(), claim.turn_id(), &context.step_id, output)
            .unwrap();
    }
    token.cancel();
    let (facts, proposals) = batch.into_parts();
    let count = facts.len();
    if proposals.is_empty() {
        assert!(facts.is_empty());
    } else {
        kernel
            .commit_domains(
                claim,
                DomainMutation {
                    request_id: DomainRequestId::new(request).unwrap(),
                    proposals,
                    facts,
                },
            )
            .await
            .unwrap();
    }
    count
}

#[tokio::test]
async fn queued_direct_turns_each_invoke_only_their_own_skill_once() {
    let fixture = Fixture::new(snapshot(false)).await;
    let store = Arc::new(SqliteStore::open(fixture.temp.path().join("store")).unwrap());
    let kernel = AgentKernel::recover(store.clone(), fixture.service.clone())
        .await
        .unwrap();
    let workers = kernel.start_workers();
    let lease = kernel.register("fixture".into()).unwrap();
    let first = fixture
        .submit(&kernel, "queued", "first", "/first", false)
        .await;
    let second = kernel
        .submit(SubmitTurn {
            session: SubmitSession::Resume(
                kernel.prepare_resume(first.session_id()).await.unwrap(),
            ),
            turn_id: TurnId::new("second").unwrap(),
            text: "/second".into(),
            model: None,
            sandbox: None,
        })
        .await
        .unwrap();
    assert_eq!(enter(&kernel, &first, "first-input").await, 1);
    assert_eq!(enter(&kernel, &first, "first-repeat").await, 0);
    assert_eq!(
        *fixture.source.requests.lock().unwrap(),
        [vec!["first".to_owned()], vec![]]
    );
    kernel
        .finish_turn(&first, &TurnOutcome::Completed)
        .await
        .unwrap();
    let claim = kernel
        .claim("fixture", CancellationToken::new())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(claim.turn_id(), &second.turn_id);
    assert_eq!(enter(&kernel, &claim, "second-input").await, 1);
    assert_eq!(enter(&kernel, &claim, "second-repeat").await, 0);
    assert_eq!(
        *fixture.source.requests.lock().unwrap(),
        [
            vec!["first".to_owned()],
            vec![],
            vec!["second".to_owned()],
            vec![]
        ]
    );
    kernel
        .finish_turn(&claim, &TurnOutcome::Completed)
        .await
        .unwrap();
    drop(lease);
    kernel.shutdown(workers).await.unwrap();
    drop(kernel);
    drop(store);
    SqliteStore::verify(fixture.temp.path().join("store")).unwrap();
    fixture.close().await;
}

#[tokio::test]
async fn workspace_last_good_tombstone_and_dedup_are_atomic_domain_behavior() {
    let fixture = Fixture::new(snapshot(true)).await;
    let store = Arc::new(SqliteStore::open(fixture.temp.path().join("store")).unwrap());
    let kernel = AgentKernel::recover(store.clone(), fixture.service.clone())
        .await
        .unwrap();
    let workers = kernel.start_workers();
    let lease = kernel.register("fixture".into()).unwrap();
    let claim = fixture
        .submit(&kernel, "workspace", "turn", "/manual", false)
        .await;
    assert_eq!(enter(&kernel, &claim, "initial").await, 2);
    let before = kernel.domain_states(claim.session_id()).await.unwrap();
    let mut incomplete = snapshot(false);
    incomplete.complete = false;
    *fixture.source.snapshot.lock().unwrap() = incomplete;
    assert_eq!(enter(&kernel, &claim, "incomplete").await, 0);
    assert_eq!(
        kernel.domain_states(claim.session_id()).await.unwrap(),
        before
    );
    *fixture.source.snapshot.lock().unwrap() = snapshot(false);
    assert_eq!(enter(&kernel, &claim, "tombstone").await, 1);
    assert_eq!(enter(&kernel, &claim, "unchanged").await, 0);
    let facts = store
        .read_facts(claim.session_id(), 0, 64)
        .await
        .unwrap()
        .facts;
    assert_eq!(
        facts
            .iter()
            .filter(|fact| matches!(
                fact.body(),
                SessionFactBody::InputMessageEntered {
                    source: InputMessageSource::AgentInstructions {
                        tombstone: true,
                        ..
                    },
                    ..
                }
            ))
            .count(),
        1
    );
    assert_eq!(
        facts
            .iter()
            .filter(|fact| matches!(
                fact.body(),
                SessionFactBody::InputMessageEntered {
                    source: InputMessageSource::UserSkillInvocation { .. },
                    ..
                }
            ))
            .count(),
        1
    );
    kernel
        .finish_turn(&claim, &TurnOutcome::Completed)
        .await
        .unwrap();
    drop(lease);
    kernel.shutdown(workers).await.unwrap();
    drop(kernel);
    drop(store);
    SqliteStore::verify(fixture.temp.path().join("store")).unwrap();
    fixture.close().await;
}

#[tokio::test]
async fn cold_sqlite_resume_uses_new_codec_generation_without_replaying_workspace_inputs() {
    let fixture = Fixture::new(snapshot(true)).await;
    let root = fixture.temp.path().join("store");
    let session_id = SessionId::new("cold-workspace").unwrap();
    for resumed in [false, true] {
        let store = Arc::new(SqliteStore::open(&root).unwrap());
        let kernel = AgentKernel::recover(store.clone(), fixture.service.clone())
            .await
            .unwrap();
        let workers = kernel.start_workers();
        let lease = kernel.register("fixture".into()).unwrap();
        let claim = fixture
            .submit(
                &kernel,
                session_id.as_str(),
                if resumed { "second" } else { "first" },
                if resumed { "continue" } else { "/manual" },
                resumed,
            )
            .await;
        assert_eq!(
            enter(
                &kernel,
                &claim,
                if resumed {
                    "second-input"
                } else {
                    "first-input"
                }
            )
            .await,
            if resumed { 0 } else { 2 }
        );
        kernel
            .finish_turn(&claim, &TurnOutcome::Completed)
            .await
            .unwrap();
        drop(lease);
        kernel.shutdown(workers).await.unwrap();
        drop(kernel);
        let facts = store.read_facts(&session_id, 0, 64).await.unwrap().facts;
        assert_eq!(
            facts
                .iter()
                .filter(|fact| matches!(fact.body(), SessionFactBody::InputMessageEntered { .. }))
                .count(),
            2
        );
        drop(store);
        SqliteStore::verify(&root).unwrap();
        if !resumed {
            fs::write(&fixture.profile, source_program("new-generation")).unwrap();
        }
    }
    assert_eq!(
        *fixture.source.requests.lock().unwrap(),
        [vec!["manual".to_owned()], vec![]]
    );
    fixture.close().await;
}

#[tokio::test]
async fn initial_empty_workspace_does_not_emit_replacement_or_tombstone() {
    let fixture = Fixture::new(snapshot(false)).await;
    let store = Arc::new(SqliteStore::open(fixture.temp.path().join("store")).unwrap());
    let kernel = AgentKernel::recover(store.clone(), fixture.service.clone())
        .await
        .unwrap();
    let workers = kernel.start_workers();
    let lease = kernel.register("fixture".into()).unwrap();
    let claim = fixture
        .submit(&kernel, "empty", "first", "work", false)
        .await;
    assert_eq!(enter(&kernel, &claim, "initial").await, 0);
    let before = kernel.domain_states(claim.session_id()).await.unwrap();
    assert_eq!(enter(&kernel, &claim, "unchanged").await, 0);
    assert_eq!(
        kernel.domain_states(claim.session_id()).await.unwrap(),
        before
    );
    kernel
        .finish_turn(&claim, &TurnOutcome::Completed)
        .await
        .unwrap();
    let facts = store
        .read_facts(claim.session_id(), 0, 64)
        .await
        .unwrap()
        .facts;
    assert!(
        !facts
            .iter()
            .any(|fact| matches!(fact.body(), SessionFactBody::InputMessageEntered { .. }))
    );
    drop(lease);
    kernel.shutdown(workers).await.unwrap();
    drop(kernel);
    drop(store);
    SqliteStore::verify(fixture.temp.path().join("store")).unwrap();
    fixture.close().await;
}

#[tokio::test]
#[allow(clippy::too_many_lines)] // One parent/child lifecycle proves inherited state and independent input admission.
async fn fork_rebinds_the_inherited_cursor_and_only_new_human_input_invokes_skills() {
    use rsi_agent_session_protocol::{ContributionId, ForkTurnSelection, MessageDelivery};
    use rsi_agent_turn_protocol::{SpawnAgentRequest, SubmitMessage};
    let fixture = Fixture::new(snapshot(true)).await;
    let store = Arc::new(SqliteStore::open(fixture.temp.path().join("store")).unwrap());
    let kernel = AgentKernel::recover(store.clone(), fixture.service.clone())
        .await
        .unwrap();
    let workers = kernel.start_workers();
    let lease = kernel.register("fixture".into()).unwrap();
    let parent_header = fixture.header("parent");
    kernel
        .submit_message(SubmitMessage {
            session: SubmitSession::Fresh(
                PreparedFreshSession::new(
                    parent_header,
                    fixture
                        .service
                        .pin(&AgentPresetId::new("test-agent").unwrap())
                        .await
                        .unwrap(),
                )
                .unwrap(),
            ),
            message: AgentMessage {
                message_id: MessageId::new("parent-task").unwrap(),
                source: AgentMessageSource::Human,
                content: vec![AgentMessageContent::Text {
                    text: "parent work".into(),
                }],
                options: MessageOptions::default(),
            },
            delivery: MessageDelivery::NextTurn,
        })
        .await
        .unwrap();
    let parent = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        kernel.claim("fixture", CancellationToken::new()),
    )
    .await
    .unwrap()
    .unwrap()
    .unwrap();
    enter(&kernel, &parent, "parent-initial").await;
    let token = CancellationToken::new();
    let context = kernel
        .contribution_context(&parent, token.clone())
        .await
        .unwrap();
    let bodies = (0..20)
        .map(|index| SessionFactBody::InputMessageEntered {
            turn_id: parent.turn_id().clone(),
            step_id: context.step_id.clone(),
            source: InputMessageSource::PluginContext {
                contribution_id: ContributionId::new("fixture.history").unwrap(),
            },
            content: vec![AgentMessageContent::Text {
                text: format!("parent context {index}"),
            }],
        })
        .collect();
    kernel.publish(&parent, bodies).await.unwrap();
    token.cancel();
    drop(context);
    enter(&kernel, &parent, "parent-advanced").await;
    let parent_state = kernel.domain_states(parent.session_id()).await.unwrap();
    kernel
        .finish_turn(&parent, &TurnOutcome::Completed)
        .await
        .unwrap();
    kernel
        .submit_message(SubmitMessage {
            session: SubmitSession::Resume(
                kernel.prepare_resume(parent.session_id()).await.unwrap(),
            ),
            message: AgentMessage {
                message_id: MessageId::new("parent-invoking").unwrap(),
                source: AgentMessageSource::Human,
                content: vec![AgentMessageContent::Text {
                    text: "spawn child".into(),
                }],
                options: MessageOptions::default(),
            },
            delivery: MessageDelivery::NextTurn,
        })
        .await
        .unwrap();
    let parent = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        kernel.claim("fixture", CancellationToken::new()),
    )
    .await
    .unwrap()
    .unwrap()
    .unwrap();
    let child = kernel
        .spawn_agent(SpawnAgentRequest {
            cancellation: CancellationToken::new(),
            caller: kernel.agent_caller(&parent).unwrap(),
            child_session_id: SessionId::new("child").unwrap(),
            task_name: "child".into(),
            message_id: MessageId::new("child-task").unwrap(),
            message: "/agent-only".into(),
            fork_turns: ForkTurnSelection::All,
        })
        .await
        .unwrap();
    let claim = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        kernel.claim("fixture", CancellationToken::new()),
    )
    .await
    .unwrap()
    .unwrap()
    .unwrap();
    assert_eq!(claim.session_id(), &child.session_id);
    kernel
        .submit_message(SubmitMessage {
            session: SubmitSession::Resume(kernel.prepare_resume(&child.session_id).await.unwrap()),
            message: AgentMessage {
                message_id: MessageId::new("human-child").unwrap(),
                source: AgentMessageSource::Human,
                content: vec![AgentMessageContent::Text {
                    text: "/manual".into(),
                }],
                options: MessageOptions::default(),
            },
            delivery: MessageDelivery::NextStep,
        })
        .await
        .unwrap();
    assert_eq!(kernel.enter_pending_step_messages(&claim).await.unwrap(), 1);
    assert_eq!(enter(&kernel, &claim, "child-input").await, 1);
    assert_eq!(
        kernel.domain_states(parent.session_id()).await.unwrap(),
        parent_state
    );
    assert_eq!(
        fixture.source.requests.lock().unwrap().last().unwrap(),
        &vec!["manual".to_owned()]
    );
    let child_facts = store
        .read_facts(&child.session_id, 0, 64)
        .await
        .unwrap()
        .facts;
    assert!(!child_facts.iter().any(|fact| matches!(
        fact.body(),
        SessionFactBody::InputMessageEntered {
            source: InputMessageSource::AgentInstructions { .. },
            ..
        }
    )));
    kernel
        .finish_turn(&claim, &TurnOutcome::Completed)
        .await
        .unwrap();
    kernel
        .finish_turn(&parent, &TurnOutcome::Completed)
        .await
        .unwrap();
    drop(lease);
    kernel.shutdown(workers).await.unwrap();
    drop(kernel);
    drop(store);
    SqliteStore::verify(fixture.temp.path().join("store")).unwrap();
    fixture.close().await;
}
