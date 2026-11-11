use crate::{PreparedJournal, prepare, restore_segment};
use async_trait::async_trait;
use rsi_agent_composition_protocol::{
    ContributionError, ContributionResult, DomainHandle, Publication, PublicationObserver,
    PublicationReceipt, SessionProjection, SessionProjectionContext, ToolSettlement,
    ToolSettlementContext, ToolSettlementContributor,
};
use rsi_agent_session_protocol::{
    DomainStateValue, EffectId, ExtensionJournal, ProjectionValue, SessionFactBody, SessionId,
    TurnId,
};
use rsi_agent_store_protocol::SessionStore;
use rsi_agent_turn_protocol::AgentCallerAuthority;
use rsi_extension::Connection;
use rsi_tools_protocol::{ToolError, ToolExecution, ToolExecutor, ToolResult};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicU8, Ordering},
    },
};
use tokio_util::sync::CancellationToken;

mod command;
mod fork;
mod lanes;
mod resource;
mod step;

fn invalid(error: impl std::fmt::Display) -> ContributionError {
    ContributionError::Invalid(error.to_string())
}
fn acknowledged(value: &Value) -> bool {
    value.is_null() || *value == json!({"state":"unloaded"})
}

fn tool_error(error: impl std::fmt::Display) -> ToolError {
    ToolError::Execution(error.to_string())
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Staged {
    operation_id: String,
    expected_revision: u64,
    token: String,
    events: Vec<Value>,
    checkpoint: DomainStateValue,
    result: ToolResult,
    conclude_turn: bool,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum ToolReply {
    Staged(Box<Staged>),
    Rejected { diagnostic: String },
}

#[derive(Clone, Debug)]
struct Unobserved {
    turn: TurnId,
    effect: EffectId,
}

#[derive(Debug)]
struct Pending {
    connection: Arc<Connection>,
    session: SessionId,
    effect: EffectId,
    turn: TurnId,
    token: String,
    prepared: PreparedJournal,
    expected: Expected,
    // 0: prepared, 1: confirmed, 2: rejected, 3: unconfirmed. Only exact evidence
    // can leave 3; merely dropping the Tool runtime's retained result cannot.
    state: AtomicU8,
}
#[derive(Debug)]
enum Expected {
    Tool {
        name: String,
        arguments: Value,
        result: Box<ToolResult>,
        identity: OnceLock<rsi_tools_protocol::ToolResultIdentity>,
        conclude: bool,
    },
    Step {
        reject_input: bool,
        producer: rsi_agent_session_protocol::ContributionId,
        step: rsi_agent_session_protocol::StepId,
        instructions: String,
        tools: Vec<String>,
    },
}
impl Pending {
    async fn acknowledge(&self, outcome: &str, revision: u64) -> ContributionResult<()> {
        let result = self
            .connection
            .request(
                "operation.receipt",
                json!({
                    "sessionId": self.session.as_str(), "operationId": self.effect.as_str(),
                    "token": self.token, "outcome": outcome, "revision": revision,
                }),
                CancellationToken::new(),
            )
            .await
            .map_err(invalid)?;
        if !acknowledged(&result) {
            return Err(invalid("invalid acknowledgement response"));
        }
        Ok(())
    }
}
#[async_trait]
impl PublicationObserver for Pending {
    async fn committed(&self, receipt: &PublicationReceipt<'_>) -> ContributionResult<()> {
        if receipt.session_id != &self.session {
            return Err(invalid("publication Session mismatch"));
        }
        match &self.expected {
            Expected::Tool {
                result: expected,
                identity: retained,
                ..
            } => {
                let [fact] = receipt.facts else {
                    return Err(invalid("publication lacks its single result"));
                };
                let SessionFactBody::ToolResult {
                    effect_id,
                    identity,
                    result,
                    ..
                } = fact.body()
                else {
                    return Err(invalid("publication lacks its exact ToolResult"));
                };
                if effect_id != &self.effect
                    || result != expected.as_ref()
                    || retained.get() != Some(identity)
                {
                    return Err(invalid("publication identity or result mismatch"));
                }
            }
            Expected::Step {
                reject_input,
                producer,
                step,
                instructions,
                tools,
            } => {
                if !receipt.facts.last().is_some_and(|fact| matches!(
                    fact.body(), SessionFactBody::StepInputsPrepared { turn_id, step_id, decision }
                        if turn_id == &self.turn && step_id == step && (!*reject_input || decision.messages().is_none())
                )) {
                    return Err(invalid("publication lacks its final input decision"));
                }
                step::verify_facts(producer, step, instructions, tools, receipt.facts)?;
            }
        }
        let (_, commit) = receipt
            .domain_commit
            .ok_or_else(|| invalid("missing atomic journal commit"))?;
        let proposal = self.prepared.proposal();
        if commit
            .request_id()
            .map(rsi_agent_session_protocol::DomainRequestId::as_str)
            != Some(self.effect.as_str())
            || !matches!(commit.source(), rsi_agent_session_protocol::DomainMutationSource::Turn { turn_id } if turn_id == &self.turn)
            || commit
                .clone()
                .with_facts(receipt.facts.iter().map(AsRef::as_ref))
                .map_err(invalid)?
                != *commit
            || !commit.updates().iter().any(|update| {
                update.snapshot() == proposal.snapshot()
                    && update.expected_revision() == proposal.expected_revision()
            })
        {
            return Err(invalid("publication does not prove the staged journal"));
        }
        self.acknowledge(
            "committed",
            proposal.expected_revision().next().map_err(invalid)?.get(),
        )
        .await?;
        self.state.store(1, Ordering::Release);
        Ok(())
    }
    async fn rejected(&self) -> ContributionResult<()> {
        self.acknowledge(
            "rejected",
            self.prepared.proposal().expected_revision().get(),
        )
        .await?;
        self.state.store(2, Ordering::Release);
        Ok(())
    }
    fn unconfirmed(&self) {
        self.state.store(3, Ordering::Release);
    }
}

/// Agent-owned dispatch and retained settlement for one external process generation.
#[derive(Debug)]
pub struct AgentAdapter {
    connection: Arc<Connection>,
    store: Arc<dyn SessionStore>,
    turns: Arc<dyn rsi_agent_turn_protocol::TurnExecution>,
    handle: DomainHandle<ExtensionJournal>,
    artifact_sha256: String,
    pending: Mutex<BTreeMap<SessionId, Arc<Pending>>>,
    unobserved: Mutex<BTreeMap<SessionId, Unobserved>>,
    // This only serializes staging. Waiting for a question occurs in the process;
    // commands must use their own Session transaction admission, not this lock.
    staging: Mutex<BTreeMap<SessionId, Arc<tokio::sync::Mutex<()>>>>,
    commands: command::Commands,
    read_budget: rsi_api_protocol::ByteBudget,
    history: Arc<crate::history::HistoryRouter>,
    _history_registrations: [rsi_extension::HandlerRegistration; 2],
    questions: Mutex<
        Option<(
            Arc<crate::questions::QuestionRouter>,
            rsi_extension::HandlerRegistration,
        )>,
    >,
}
impl AgentAdapter {
    /// Binds an exact generation and Store; no global discovery or publication authority.
    ///
    /// # Errors
    /// Rejects an invalid local byte budget.
    pub fn new(
        connection: Arc<Connection>,
        store: Arc<dyn SessionStore>,
        turns: Arc<dyn rsi_agent_turn_protocol::TurnExecution>,
        handle: DomainHandle<ExtensionJournal>,
        artifact_sha256: String,
    ) -> Result<Arc<Self>, crate::Error> {
        crate::definition(artifact_sha256.clone())?;
        let history = Arc::new(crate::history::HistoryRouter::default());
        let register = |method| {
            connection
                .register_handler(method, history.clone())
                .map_err(crate::error)
        };
        let registrations = [
            register("agent.step-decision.read")?,
            register("agent.history.read")?,
        ];
        Ok(Arc::new(Self {
            history,
            _history_registrations: registrations,
            connection,
            store,
            turns,
            handle,
            artifact_sha256,
            questions: Mutex::new(None),
            pending: Mutex::new(BTreeMap::new()),
            unobserved: Mutex::new(BTreeMap::new()),
            staging: Mutex::new(BTreeMap::new()),
            commands: command::Commands::default(),
            read_budget: rsi_api_protocol::ByteBudget::new(64 * 1024 * 1024)
                .map_err(crate::error)?,
        }))
    }
    /// Creates an executor for an already admitted immutable Tool definition.
    pub fn tool(self: &Arc<Self>, name: String) -> Arc<dyn ToolExecutor> {
        Arc::new(ExternalTool {
            adapter: self.clone(),
            name,
        })
    }
    /// Installs the optional exact-call human question authority once per generation.
    ///
    /// # Errors
    /// Rejects duplicate registration or a closed process generation.
    pub fn enable_questions(
        &self,
        questions: Arc<dyn rsi_user_questions_protocol::UserQuestions>,
    ) -> Result<(), crate::Error> {
        let mut slot = self
            .questions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if slot.is_some() {
            return Err(crate::error("question channel already installed"));
        }
        let router = crate::questions::QuestionRouter::new(self.turns.clone(), questions);
        let registration = self
            .connection
            .register_handler("user-question.ask", router.clone())
            .map_err(crate::error)?;
        *slot = Some((router, registration));
        Ok(())
    }
    async fn reconcile(&self, operation: &Pending) -> rsi_tools_protocol::Result<()> {
        let request = rsi_agent_session_protocol::DomainRequestId::new(operation.effect.as_str())
            .map_err(tool_error)?;
        if let Some(record) = self
            .store
            .read_domain_request(&operation.session, &request)
            .await
            .map_err(|_| ToolError::OutcomeUnknown)?
        {
            let rsi_agent_session_protocol::AgentControlRecordBody::DomainStateCommitted { commit } =
                record.body()
            else {
                return Err(ToolError::OutcomeUnknown);
            };
            let mut facts = Vec::new();
            if let Some(span) = commit.fact_span() {
                let count = usize::try_from(span.count()).map_err(|_| ToolError::OutcomeUnknown)?;
                if count > rsi_agent_session_protocol::MAXIMUM_FACTS_PER_READ {
                    return Err(ToolError::OutcomeUnknown);
                }
                let mut cursor = span.first_seq() - 1;
                while facts.len() < count {
                    let previous = cursor;
                    let page = self
                        .store
                        .read_facts(
                            &operation.session,
                            cursor,
                            (count - facts.len()).min(256),
                            rsi_agent_store_protocol::MAXIMUM_STORE_FACT_PAGE_BYTES,
                        )
                        .await
                        .map_err(|_| ToolError::OutcomeUnknown)?;
                    for fact in page.facts {
                        if fact.seq() != cursor + 1 {
                            return Err(ToolError::OutcomeUnknown);
                        }
                        cursor = fact.seq();
                        facts.push(Arc::new(fact));
                    }
                    if cursor == previous || facts.len() > count {
                        return Err(ToolError::OutcomeUnknown);
                    }
                }
            }
            operation
                .committed(&PublicationReceipt {
                    session_id: &operation.session,
                    facts: &facts,
                    domain_commit: Some((record.seq(), commit)),
                })
                .await
                .map_err(|_| ToolError::OutcomeUnknown)
        } else {
            // Absence alone cannot race an owned Kernel commit. A terminal boundary
            // fences every domain mutation belonging to this originating Turn.
            let boundary = self
                .store
                .read_turn_boundary(&operation.session, &operation.turn)
                .await
                .map_err(|_| ToolError::OutcomeUnknown)?;
            if boundary.terminal().is_none() {
                return Err(ToolError::OutcomeUnknown);
            }
            // Re-read after observing the fence, never before it.
            if self
                .store
                .read_domain_request(&operation.session, &request)
                .await
                .map_err(|_| ToolError::OutcomeUnknown)?
                .is_some()
            {
                return Err(ToolError::OutcomeUnknown);
            }
            operation
                .rejected()
                .await
                .map_err(|_| ToolError::OutcomeUnknown)
        }
    }
    async fn reconcile_unobserved(&self, session: &SessionId) -> rsi_tools_protocol::Result<()> {
        let prior = self
            .unobserved
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(session)
            .cloned();
        let Some(prior) = prior else {
            return Ok(());
        };
        let boundary = self
            .store
            .read_turn_boundary(session, &prior.turn)
            .await
            .map_err(|_| ToolError::OutcomeUnknown)?;
        if boundary.terminal().is_none() {
            return Err(ToolError::OutcomeUnknown);
        }
        let request = rsi_agent_session_protocol::DomainRequestId::new(prior.effect.as_str())
            .map_err(tool_error)?;
        if self
            .store
            .read_domain_request(session, &request)
            .await
            .map_err(|_| ToolError::OutcomeUnknown)?
            .is_some()
        {
            return Err(ToolError::OutcomeUnknown);
        }
        let acknowledged = self
            .connection
            .request(
                "operation.abandon",
                json!({"sessionId": session,
            "operationId": prior.effect.as_str()}),
                CancellationToken::new(),
            )
            .await
            .map_err(|_| ToolError::OutcomeUnknown)?;
        if !acknowledged.is_null() {
            return Err(ToolError::OutcomeUnknown);
        }
        self.unobserved
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(session);
        Ok(())
    }
    #[allow(clippy::too_many_lines)] // One staged operation retains authority, payload and CAS ownership across its failure boundaries.
    fn staging_lane(
        &self,
        session: &SessionId,
    ) -> rsi_tools_protocol::Result<Arc<tokio::sync::Mutex<()>>> {
        lanes::acquire(&self.staging, session)
            .ok_or_else(|| tool_error("extension Session capacity"))
    }

    async fn load_current(
        &self,
        header: &rsi_agent_session_protocol::SessionHeader,
        expected: Option<rsi_agent_session_protocol::DomainRevision>,
        cancellation: CancellationToken,
    ) -> rsi_tools_protocol::Result<(rsi_agent_session_protocol::DomainRevision, ExtensionJournal)>
    {
        self.reconcile_commands(header.session_id())
            .await
            .map_err(tool_error)?;
        self.reconcile_unobserved(header.session_id()).await?;
        let previous = self
            .pending
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(header.session_id())
            .cloned();
        if let Some(previous) = &previous
            && !matches!(previous.state.load(Ordering::Acquire), 1 | 2)
        {
            self.reconcile(previous).await?;
        }
        {
            let mut pending = self
                .pending
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if let Some(previous) = pending.get(header.session_id()) {
                if !matches!(previous.state.load(Ordering::Acquire), 1 | 2) {
                    return Err(ToolError::OutcomeUnknown);
                }
                pending.remove(header.session_id());
            }
            pending
                .retain(|_, operation| !matches!(operation.state.load(Ordering::Acquire), 1 | 2));
            if pending.len() >= 256 {
                return Err(tool_error("extension Session capacity"));
            }
        }
        let page = self
            .store
            .read_domain_states(
                header.session_id(),
                None,
                Some(rsi_agent_session_protocol::EXTENSION_JOURNAL_DOMAIN),
            )
            .await
            .map_err(tool_error)?;
        let view = page
            .states
            .into_iter()
            .next()
            .ok_or_else(|| tool_error("extension journal is absent"))?;
        if expected.is_some_and(|revision| revision != view.head.revision) {
            return Err(tool_error("journal changed after Step capture"));
        }
        let journal = self.handle.decode(&view.snapshot).map_err(tool_error)?;
        self.load_snapshot(header, view.head.revision, &journal, cancellation)
            .await?;
        Ok((view.head.revision, journal))
    }
    async fn load_snapshot(
        &self,
        header: &rsi_agent_session_protocol::SessionHeader,
        revision: rsi_agent_session_protocol::DomainRevision,
        journal: &ExtensionJournal,
        cancellation: CancellationToken,
    ) -> rsi_tools_protocol::Result<()> {
        if journal.artifact_sha256 != self.artifact_sha256 {
            return Err(tool_error("journal artifact cohort mismatch"));
        }
        let transferred = async {
            let count: u64 = journal.segments.iter().map(|segment| segment.count).sum();
            let local_cwd = (header.coordinates().location() == &rsi_execution::ExecutionLocation::Local)
                .then(|| header.canonical_cwd());
            let boundary = json!({
                "session_id": header.session_id(), "created_at": header.created_at_ms(),
                "local_cwd": local_cwd, "revision": revision.get(), "count": count,
            });
            let action = self.connection.request("agent.load.begin", boundary, cancellation.clone())
                .await.map_err(tool_error)?;
            match action.as_str() {
                Some("current") if revision.get() > 0 => return Ok(()),
                Some("transfer") => {}
                _ => return Err(tool_error("invalid journal load disposition")),
            }
            for segment in &journal.segments {
                let events = restore_segment(self.store.as_ref(), segment, self.read_budget.clone().into())
                    .await.map_err(tool_error)?;
                let page = json!({"session_id": header.session_id(), "offset": segment.first, "events": events});
                self.connection.request("agent.load.events", page, cancellation.clone())
                    .await.map_err(tool_error)?;
            }
            let finish = json!({"session_id": header.session_id(), "checkpoint": journal.checkpoint});
            self.connection.request("agent.load.finish", finish, cancellation.clone())
                .await.map_err(tool_error)?;
            Ok(())
        }.await;
        if transferred.is_err() {
            let _ = self
                .connection
                .request(
                    "agent.load.abort",
                    json!({"session_id": header.session_id()}),
                    CancellationToken::new(),
                )
                .await;
        }
        transferred
    }
    #[allow(clippy::too_many_lines)] // Keep exact Tool authority, external outcome classification and atomic ownership transfer together.
    async fn execute(
        &self,
        name: &str,
        arguments: Value,
        execution: ToolExecution,
    ) -> rsi_tools_protocol::Result<ToolResult> {
        let caller = execution
            .extension::<AgentCallerAuthority>()
            .ok_or_else(|| tool_error("missing Agent caller authority"))?;
        let effect = caller
            .tool_effect_id()
            .ok_or_else(|| tool_error("missing exact Tool effect"))?;
        let lane = self.staging_lane(caller.session_id())?;
        let _staging = tokio::select! {
            () = execution.cancellation.cancelled() => return Err(ToolError::Cancelled),
            guard = lane.lock() => guard,
        };
        let (revision, journal) = self
            .load_current(caller.header(), None, execution.cancellation.clone())
            .await?;
        let captured = self
            .turns
            .tool_history(&caller, execution.cancellation.clone())
            .await
            .map_err(tool_error)?;
        let _history_scope = self
            .history
            .enter_reader(
                caller.session_id().as_str(),
                effect.as_str(),
                captured.facts,
                captured.through_seq,
                &execution.cancellation,
            )
            .map_err(tool_error)?;
        self.unobserved
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(
                caller.session_id().clone(),
                Unobserved {
                    effect: effect.clone(),
                    turn: caller.turn_id().clone(),
                },
            );
        let _question_scope = self
            .questions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_ref()
            .map(|(router, _)| router.enter(&execution))
            .transpose()
            .map_err(tool_error)?;
        let response = self.connection.request("agent.tool", json!({
            "session_id": caller.session_id(), "operation_id": effect.as_str(), "name": name, "arguments": arguments, "fact_horizon": captured.through_seq,
        }), execution.cancellation.clone()).await.map_err(|_| ToolError::OutcomeUnknown)?;
        let staged = match serde_json::from_value::<ToolReply>(response)
            .map_err(|_| ToolError::OutcomeUnknown)?
        {
            ToolReply::Staged(staged) => staged,
            ToolReply::Rejected { diagnostic } => {
                if diagnostic.is_empty() || diagnostic.len() > 4096 {
                    return Err(ToolError::OutcomeUnknown);
                }
                self.unobserved
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .remove(caller.session_id());
                return Err(tool_error(diagnostic));
            }
        };
        if staged.operation_id != effect.as_str()
            || staged.expected_revision != revision.get()
            || staged.token.len() != 64
            || !staged.token.bytes().all(|byte| byte.is_ascii_hexdigit())
        {
            return Err(ToolError::OutcomeUnknown);
        }
        let prepared = prepare(
            self.store.as_ref(),
            &self.handle,
            revision,
            &journal,
            staged.events,
            staged.checkpoint,
            self.read_budget.clone().into(),
        )
        .await;
        let prepared = match prepared {
            Ok(prepared) => prepared,
            Err(error) => {
                self.connection.request("operation.receipt", json!({"sessionId": caller.session_id(),
                    "operationId": effect.as_str(), "token": staged.token, "outcome": "rejected", "revision": revision.get()}),
                    CancellationToken::new()).await.map_err(|_| ToolError::OutcomeUnknown)?;
                self.unobserved
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .remove(caller.session_id());
                return Err(tool_error(error));
            }
        };
        let result = staged.result.clone();
        self.pending
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(
                caller.session_id().clone(),
                Arc::new(Pending {
                    connection: self.connection.clone(),
                    session: caller.session_id().clone(),
                    effect: effect.clone(),
                    turn: caller.turn_id().clone(),
                    token: staged.token,
                    prepared,
                    expected: Expected::Tool {
                        name: name.into(),
                        arguments,
                        result: Box::new(staged.result),
                        conclude: staged.conclude_turn,
                        identity: OnceLock::new(),
                    },
                    state: AtomicU8::new(0),
                }),
            );
        self.unobserved
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(caller.session_id());
        Ok(result)
    }
}

#[derive(Debug)]
struct ExternalTool {
    adapter: Arc<AgentAdapter>,
    name: String,
}
#[async_trait]
impl ToolExecutor for ExternalTool {
    async fn execute(
        &self,
        arguments: Value,
        execution: ToolExecution,
    ) -> rsi_tools_protocol::Result<ToolResult> {
        self.adapter.execute(&self.name, arguments, execution).await
    }
}
impl ToolSettlementContributor for AgentAdapter {
    fn settle(&self, context: &ToolSettlementContext<'_>) -> ContributionResult<ToolSettlement> {
        let SessionFactBody::ToolIntent {
            effect_id,
            identity,
            name,
            arguments,
            ..
        } = context.intent.body()
        else {
            return Err(invalid("missing ToolIntent"));
        };
        let pending = self
            .pending
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(operation) = pending
            .get(context.header.session_id())
            .filter(|operation| &operation.effect == effect_id)
        else {
            return Ok(ToolSettlement::default());
        };
        let Expected::Tool {
            name: expected_name,
            arguments: expected_arguments,
            result: expected_result,
            identity: retained,
            conclude,
        } = &operation.expected
        else {
            return Ok(ToolSettlement::default());
        };
        if expected_name != name
            || expected_arguments != arguments
            || expected_result.as_ref() != context.result
            || operation.state.load(Ordering::Acquire) != 0
        {
            return Err(invalid("retained extension result mismatch"));
        }
        if retained.get_or_init(|| identity.clone()) != identity {
            return Err(invalid("retained Tool identity changed"));
        }
        Ok(ToolSettlement {
            publications: vec![Publication::new(operation.clone())],
            domains: vec![operation.prepared.proposal().clone()],
            require_uncancelled_turn: true,
            conclusion: conclude
                .then_some(rsi_agent_session_protocol::ToolConclusion { structured: None }),
        })
    }
}
#[async_trait]
impl SessionProjection for AgentAdapter {
    async fn project(
        &self,
        context: &SessionProjectionContext,
        cancellation: CancellationToken,
    ) -> ContributionResult<ProjectionValue> {
        if cancellation.is_cancelled() {
            return Err(ContributionError::Closed);
        }
        let view = context
            .domains()
            .iter()
            .find(|view| {
                view.snapshot.identity().id()
                    == rsi_agent_session_protocol::EXTENSION_JOURNAL_DOMAIN
            })
            .ok_or_else(|| invalid("extension journal is absent"))?;
        let journal = self.handle.decode(&view.snapshot).map_err(invalid)?;
        ProjectionValue::new(journal.checkpoint.value().clone()).map_err(invalid)
    }
}
