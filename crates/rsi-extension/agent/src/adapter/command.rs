use super::{
    AgentAdapter, Arc, AtomicU8, BTreeMap, CancellationToken, Connection, ContributionError,
    ContributionResult, Deserialize, DomainStateValue, ExtensionJournal, Mutex, Ordering,
    PreparedJournal, Publication, PublicationObserver, PublicationReceipt, SessionId, Value,
    async_trait, invalid, json, prepare,
};
use rsi_agent_composition_protocol::{
    CommandInput, CommandInputDelivery, CommandOutcome, CommandOutcomeEvidence,
    DraftPublicationReceipt, PreparedCommandInput, SessionCommand, SessionCommandContext,
    SessionCommandOutput,
};
use rsi_agent_session_protocol::{
    CommandArguments, ContributionId, DomainMutationSource, SessionCommandInvocation,
};

#[derive(Debug, Default)]
pub(super) struct Commands {
    lanes: Mutex<BTreeMap<SessionId, Arc<tokio::sync::Mutex<()>>>>,
    pending: Mutex<BTreeMap<SessionId, Arc<PendingCommand>>>,
    unobserved: Mutex<BTreeMap<SessionId, UnobservedCommand>>,
}
#[derive(Clone, Debug)]
struct UnobservedCommand {
    invocation: SessionCommandInvocation,
    outcome: CommandOutcome,
}
#[derive(Debug)]
struct PendingCommand {
    connection: Arc<Connection>,
    session: SessionId,
    invocation: SessionCommandInvocation,
    outcome: CommandOutcome,
    token: String,
    prepared: PreparedJournal,
    inputs: Vec<PreparedCommandInput>,
    state: AtomicU8,
}
impl PendingCommand {
    async fn acknowledge(&self, outcome: &str, revision: u64) -> ContributionResult<()> {
        let response = self
            .connection
            .request(
                "operation.receipt",
                json!({
                    "sessionId": self.session, "operationId": self.invocation.request_id,
                    "token": self.token, "outcome": outcome, "revision": revision,
                }),
                CancellationToken::new(),
            )
            .await
            .map_err(invalid)?;
        if !super::acknowledged(&response) {
            return Err(invalid("invalid command acknowledgement"));
        }
        self.state
            .store(if outcome == "rejected" { 2 } else { 1 }, Ordering::Release);
        Ok(())
    }
    async fn reconcile(
        &self,
        store: &dyn rsi_agent_store_protocol::SessionStore,
    ) -> ContributionResult<()> {
        let Some(evidence) = self.outcome.evidence() else {
            // Canonical presence proves publication even after an uncertain Store
            // write. Absence still cannot prove rejection of an owned operation.
            let record = store
                .read_domain_request(&self.session, &self.invocation.request_id)
                .await
                .map_err(invalid)?
                .ok_or_else(|| invalid("command publication remains unresolved"))?;
            let rsi_agent_session_protocol::AgentControlRecordBody::DomainStateCommitted { commit } =
                record.body()
            else {
                return Err(invalid("invalid canonical command receipt"));
            };
            return self
                .committed(&PublicationReceipt {
                    session_id: &self.session,
                    facts: &[],
                    domain_commit: Some((record.seq(), commit)),
                })
                .await;
        };
        match evidence.as_ref() {
            CommandOutcomeEvidence::Rejected => self.rejected().await,
            CommandOutcomeEvidence::Durable {
                session_id,
                control_seq,
                commit,
            } => {
                self.committed(&PublicationReceipt {
                    session_id,
                    facts: &[],
                    domain_commit: Some((*control_seq, commit)),
                })
                .await
            }
            CommandOutcomeEvidence::Draft {
                session_id,
                receipt,
                baseline,
                inputs,
            } => {
                self.draft_committed(&DraftPublicationReceipt {
                    session_id,
                    receipt,
                    baseline,
                    inputs,
                })
                .await
            }
        }
    }
}
#[async_trait]
impl PublicationObserver for PendingCommand {
    async fn committed(&self, receipt: &PublicationReceipt<'_>) -> ContributionResult<()> {
        let (seq, commit) = receipt
            .domain_commit
            .ok_or_else(|| invalid("missing command control"))?;
        let proposal = self.prepared.proposal();
        let expected = self.inputs.iter().map(|input| (input.message(), input.publication_delivery(false)));
        if receipt.session_id != &self.session
            || !receipt.facts.is_empty()
            || !matches!(commit.source(), DomainMutationSource::Command { invocation } if invocation == &self.invocation)
            || commit.request_id() != Some(&self.invocation.request_id)
            || commit.command_inputs().iter().map(|input| (&input.message, input.delivery)).ne(expected)
            || !commit.updates().iter().any(|update| {
                update.snapshot() == proposal.snapshot()
                    && update.expected_revision() == proposal.expected_revision()
            })
        {
            return Err(invalid(
                "command publication identity, journal or inputs mismatch",
            ));
        }
        rsi_agent_session_protocol::SessionCommandReceipt::committed(seq, commit)
            .map_err(invalid)?;
        self.acknowledge(
            "committed",
            proposal.expected_revision().next().map_err(invalid)?.get(),
        )
        .await
    }
    async fn draft_committed(
        &self,
        receipt: &DraftPublicationReceipt<'_>,
    ) -> ContributionResult<()> {
        let proposal = self.prepared.proposal();
        let expected = rsi_agent_session_protocol::SessionCommandReceipt::draft_changed(
            &self.invocation,
            receipt.state_sha256()?,
        )
        .map_err(invalid)?;
        let own = receipt.inputs.iter().filter(|input| matches!(&input.message().source,
            rsi_agent_session_protocol::AgentMessageSource::Command { source } if source.request_id == self.invocation.request_id))
            .map(PreparedCommandInput::message);
        if receipt.session_id != &self.session
            || receipt.receipt != &expected
            || proposal.expected_revision().get() != 0
            || own.ne(self.inputs.iter().map(PreparedCommandInput::message))
            || !receipt
                .baseline
                .initial_states()
                .iter()
                .any(|snapshot| snapshot == proposal.snapshot())
        {
            return Err(invalid(
                "draft publication identity, journal or inputs mismatch",
            ));
        }
        self.acknowledge("draft_committed", 0).await
    }
    async fn rejected(&self) -> ContributionResult<()> {
        self.acknowledge(
            "rejected",
            self.prepared.proposal().expected_revision().get(),
        )
        .await
    }
    fn unconfirmed(&self) {
        self.state.store(3, Ordering::Release);
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct StagedCommand {
    operation_id: String,
    expected_revision: u64,
    token: String,
    events: Vec<Value>,
    checkpoint: DomainStateValue,
    inputs: Vec<Input>,
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Input {
    delivery: Delivery,
    text: String,
}
#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Delivery {
    Inject,
    Steer,
}
#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum Reply {
    Staged(Box<StagedCommand>),
    Rejected { diagnostic: String },
}

#[derive(Debug)]
struct ExternalCommand {
    adapter: Arc<AgentAdapter>,
    id: ContributionId,
    name: String,
}
impl AgentAdapter {
    /// Binds a callback to one frozen catalog entry.
    pub fn command(self: &Arc<Self>, id: ContributionId, name: String) -> Arc<dyn SessionCommand> {
        Arc::new(ExternalCommand {
            adapter: self.clone(),
            id,
            name,
        })
    }
    fn command_lane(&self, session: &SessionId) -> ContributionResult<Arc<tokio::sync::Mutex<()>>> {
        super::lanes::acquire(&self.commands.lanes, session).ok_or(ContributionError::Capacity)
    }

    pub(super) async fn reconcile_commands(&self, session: &SessionId) -> ContributionResult<()> {
        self.commands
            .pending
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .retain(|_, operation| !matches!(operation.state.load(Ordering::Acquire), 1 | 2));
        let pending = self
            .commands
            .pending
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(session)
            .cloned();
        if let Some(pending) = pending {
            if !matches!(pending.state.load(Ordering::Acquire), 1 | 2) {
                pending.reconcile(self.store.as_ref()).await?;
            }
            let mut entries = self
                .commands
                .pending
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if entries
                .get(session)
                .is_some_and(|current| Arc::ptr_eq(current, &pending))
            {
                entries.remove(session);
            }
        }
        let unobserved = self
            .commands
            .unobserved
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(session)
            .cloned();
        if let Some(unobserved) = unobserved {
            if !matches!(
                unobserved.outcome.evidence().as_deref(),
                Some(CommandOutcomeEvidence::Rejected)
            ) {
                return Err(invalid(
                    "command reply was lost and publication remains unresolved",
                ));
            }
            let response = self
                .connection
                .request(
                    "operation.abandon",
                    json!({
                        "sessionId": session, "operationId": unobserved.invocation.request_id,
                    }),
                    CancellationToken::new(),
                )
                .await
                .map_err(invalid)?;
            if !response.is_null() {
                return Err(invalid("invalid command abandonment"));
            }
            let mut entries = self
                .commands
                .unobserved
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if entries
                .get(session)
                .is_some_and(|current| current.invocation == unobserved.invocation)
            {
                entries.remove(session);
            }
        }
        Ok(())
    }
}
#[async_trait]
impl SessionCommand for ExternalCommand {
    async fn execute(
        &self,
        context: &SessionCommandContext,
        arguments: &CommandArguments,
        cancellation: CancellationToken,
    ) -> ContributionResult<SessionCommandOutput> {
        let outcome = context
            .outcome
            .clone()
            .ok_or_else(|| invalid("command lacks publication owner"))?;
        let input: String = serde_json::from_value(arguments.value().clone()).map_err(invalid)?;
        let invocation = SessionCommandInvocation {
            command: self.id.clone(),
            request_id: context.request_id.clone(),
            expected_revision: context.revision,
            arguments: arguments.clone(),
        };
        let session = context.header.session_id();
        let lane = self.adapter.command_lane(session)?;
        let _lane = tokio::select! {
            () = cancellation.cancelled() => return Err(ContributionError::Closed),
            guard = lane.lock() => guard,
        };
        self.adapter.reconcile_commands(session).await?;
        let view = context
            .domains
            .iter()
            .find(|view| {
                view.snapshot.identity().id()
                    == rsi_agent_session_protocol::EXTENSION_JOURNAL_DOMAIN
            })
            .ok_or_else(|| invalid("missing captured command journal"))?;
        let journal = self
            .adapter
            .handle
            .decode(&view.snapshot)
            .map_err(invalid)?;
        self.adapter
            .load_snapshot(
                &context.header,
                view.revision,
                &journal,
                cancellation.clone(),
            )
            .await
            .map_err(invalid)?;
        self.adapter
            .commands
            .unobserved
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(
                session.clone(),
                UnobservedCommand {
                    invocation: invocation.clone(),
                    outcome: outcome.clone(),
                },
            );
        let _history = context
            .history
            .as_ref()
            .map(|history| {
                self.adapter.history.enter_reader(
                    session.as_str(),
                    context.request_id.as_str(),
                    history.facts.clone(),
                    history.through_seq,
                    &cancellation,
                )
            })
            .transpose()
            .map_err(invalid)?;
        let response = self.adapter.connection.request("agent.command", json!({
            "session_id": session, "operation_id": context.request_id, "name": self.name, "input": input,
            "fact_horizon": context.history.as_ref().map_or(0, |history| history.through_seq),
        }), cancellation).await.map_err(invalid)?;
        let staged = match serde_json::from_value::<Reply>(response).map_err(invalid)? {
            Reply::Staged(staged) => staged,
            Reply::Rejected { diagnostic } => {
                if diagnostic.is_empty() || diagnostic.len() > 4096 {
                    return Err(invalid("invalid command rejection"));
                }
                self.adapter
                    .commands
                    .unobserved
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .remove(session);
                return Err(invalid(diagnostic));
            }
        };
        self.stage(
            context,
            invocation,
            outcome,
            view.revision,
            &journal,
            *staged,
        )
        .await
    }
}
impl ExternalCommand {
    async fn stage(
        &self,
        context: &SessionCommandContext,
        invocation: SessionCommandInvocation,
        outcome: CommandOutcome,
        revision: rsi_agent_session_protocol::DomainRevision,
        journal: &ExtensionJournal,
        staged: StagedCommand,
    ) -> ContributionResult<SessionCommandOutput> {
        if staged.operation_id != invocation.request_id.as_str()
            || staged.expected_revision != revision.get()
            || staged.token.len() != 64
            || !staged.token.bytes().all(|byte| byte.is_ascii_hexdigit())
        {
            return Err(invalid("staged command identity mismatch"));
        }
        let inputs: Vec<_> = staged
            .inputs
            .into_iter()
            .map(|input| CommandInput {
                delivery: match input.delivery {
                    Delivery::Inject => CommandInputDelivery::Inject,
                    Delivery::Steer => CommandInputDelivery::Steer,
                },
                content: vec![rsi_agent_session_protocol::AgentMessageContent::Text {
                    text: input.text,
                }],
                attribution: None,
            })
            .collect();
        // The pending observer retains its own expectation across a lost host reply.
        let prepared_inputs = PreparedCommandInput::prepare(&invocation, inputs.clone())?;
        let prepared = prepare(
            self.adapter.store.as_ref(),
            &self.adapter.handle,
            revision,
            journal,
            staged.events,
            staged.checkpoint,
            self.adapter.read_budget.clone().into(),
        )
        .await
        .map_err(invalid)?;
        let operation = Arc::new(PendingCommand {
            connection: self.adapter.connection.clone(),
            session: context.header.session_id().clone(),
            invocation,
            outcome,
            token: staged.token,
            prepared,
            inputs: prepared_inputs,
            state: AtomicU8::new(0),
        });
        self.adapter
            .commands
            .pending
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(operation.session.clone(), operation.clone());
        self.adapter
            .commands
            .unobserved
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&operation.session);
        Ok(SessionCommandOutput {
            domains: vec![operation.prepared.proposal().clone()],
            inputs,
            publications: vec![Publication::new(operation)],
        })
    }
}
