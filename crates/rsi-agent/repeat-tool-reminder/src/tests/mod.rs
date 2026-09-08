use super::*;
use rsi_agent_composition_protocol::{
    ContributionFactPage, ContributionFactReader, ContributionHorizon, DomainCatalog,
};
use rsi_agent_session_protocol::{
    AgentMessageContent, AgentPresetId, DomainRevision, DomainStateView, FrozenAgentSettings,
    MessageId, SessionHeader, StepId,
};
use serde_json::json;
use std::sync::Mutex;

mod behavior;
mod rejection;

#[derive(Debug)]
struct Reader {
    facts: Vec<Arc<SessionFact>>,
    reads: Mutex<Vec<u64>>,
}
#[async_trait]
impl ContributionFactReader for Reader {
    async fn read(&self, after: u64, limit: usize) -> ContributionResult<ContributionFactPage> {
        assert_eq!(limit, 1, "one bounded retained Fact per read");
        self.reads.lock().unwrap().push(after);
        let facts: Vec<_> = self
            .facts
            .iter()
            .filter(|f| f.seq() > after)
            .take(limit)
            .cloned()
            .collect();
        Ok(ContributionFactPage {
            through_seq: facts.last().map_or(after, |f| f.seq()),
            facts,
        })
    }
}
struct Fixture {
    reminder: Reminder,
    context: ContributionContext,
    facts: Vec<Arc<SessionFact>>,
}
impl Fixture {
    fn new(config: Config) -> Self {
        let definition = DomainDefinition::new(
            DomainIdentity::new(DOMAIN, 1).unwrap(),
            &State::default(),
            State::validate,
        )
        .unwrap();
        let catalog = DomainCatalog::new([definition.registration()]).unwrap();
        let state = catalog.bind(&definition).unwrap();
        Self {
            reminder: Reminder { state, config },
            context: ContributionContext {
                header: Arc::new(header("session")),
                turn_id: TurnId::new("turn").unwrap(),
                accepted_fact_seq: 1,
                step_id: StepId::new("step").unwrap(),
                horizon: ContributionHorizon {
                    fact_seq: 0,
                    control_seq: 1,
                },
                domains: vec![DomainStateView {
                    revision: DomainRevision::new(1),
                    snapshot: catalog.baseline()[0].clone(),
                }]
                .into(),
                facts: Arc::new(Reader {
                    facts: vec![],
                    reads: Mutex::default(),
                }),
            },
            facts: vec![],
        }
    }
    fn append(&mut self, body: SessionFactBody) -> Arc<SessionFact> {
        let fact = Arc::new(SessionFact::new(self.facts.len() as u64 + 1, 1, body).unwrap());
        self.facts.push(fact.clone());
        fact
    }
    fn call(&mut self, name: &str, arguments: ConfigValue) -> Arc<SessionFact> {
        let effect_id = EffectId::new(format!("effect-{}", self.facts.len())).unwrap();
        let identity = ToolResultIdentity::new(
            "owner",
            format!("invocation-{}", self.facts.len()),
            "call",
            "a".repeat(64),
        )
        .unwrap();
        self.append(SessionFactBody::ToolIntent {
            turn_id: self.context.turn_id.clone(),
            effect_id: effect_id.clone(),
            identity: identity.clone(),
            name: name.into(),
            arguments,
            approval: None,
            parallel_safe: true,
        });
        self.append(SessionFactBody::ToolResult {
            turn_id: self.context.turn_id.clone(),
            effect_id,
            identity,
            result: rsi_tools_protocol::ToolResult::new(json!({"exit_code":7}), vec![], true)
                .unwrap(),
        })
    }
    fn input(&mut self, human: bool) {
        self.append(SessionFactBody::InputMessageEntered {
            turn_id: self.context.turn_id.clone(),
            step_id: self.context.step_id.clone(),
            source: if human {
                InputMessageSource::Human {
                    message_id: MessageId::new(format!("message-{}", self.facts.len())).unwrap(),
                }
            } else {
                InputMessageSource::PluginContext {
                    contribution_id: ContributionId::new("fixture.other").unwrap(),
                }
            },
            content: vec![AgentMessageContent::Text {
                text: "next".into(),
            }],
        });
    }
    fn capture(&mut self) -> Arc<Reader> {
        let reader = Arc::new(Reader {
            facts: self.facts.clone(),
            reads: Mutex::default(),
        });
        self.context.horizon.fact_seq = self.facts.last().map_or(0, |f| f.seq());
        self.context.facts = reader.clone();
        reader
    }
    async fn run(&mut self, settled: &[Arc<SessionFact>]) -> ContributionOutput {
        self.capture();
        self.reminder
            .contribute(&self.context, settled, CancellationToken::new())
            .await
            .unwrap()
    }
    fn apply(&mut self, output: &ContributionOutput) {
        assert_eq!(output.domains.len(), 1);
        let proposal = &output.domains[0];
        assert_eq!(
            proposal.expected_revision(),
            self.context.domains[0].revision
        );
        self.context.domains = vec![DomainStateView {
            revision: DomainRevision::new(self.context.domains[0].revision.get() + 1),
            snapshot: proposal.snapshot().clone(),
        }]
        .into();
    }
    fn state(&self) -> State {
        self.reminder
            .state
            .decode(&self.context.domains[0].snapshot)
            .unwrap()
    }
}
fn header(id: &str) -> SessionHeader {
    SessionHeader::new(
        SessionId::new(id).unwrap(),
        1,
        "/workspace",
        AgentPresetId::new("fixture").unwrap(),
        FrozenAgentSettings::new(
            "settings",
            "system",
            rsi_ai_protocol::ModelRef::new("fixture", "model").unwrap(),
            rsi_sandbox::SandboxMode::ReadOnly,
            false,
        )
        .unwrap(),
    )
    .unwrap()
}
