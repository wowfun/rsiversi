use super::{
    AgentAdapter, Arc, AtomicU8, CancellationToken, ContributionError, ContributionResult,
    Deserialize, DomainStateValue, EffectId, Expected, Pending, Publication, Unobserved, Value,
    async_trait, invalid, json,
};
use rsi_agent_composition_protocol::{
    BeforeStepContext, BeforeStepContributor, BeforeStepOutput, ContributionOutput,
};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct StagedStep {
    operation_id: String,
    expected_revision: u64,
    token: String,
    events: Vec<Value>,
    checkpoint: DomainStateValue,
    instructions: String,
    decision: DecisionReply,
    tools: Vec<String>,
}
#[derive(Deserialize)]
#[serde(untagged, deny_unknown_fields)]
enum DecisionReply {
    Inline(rsi_agent_session_protocol::StepInputDecision),
    Paged { bytes: usize },
}

impl DecisionReply {
    async fn resolve(
        self,
        adapter: &AgentAdapter,
        session: &str,
        operation: &str,
        token: &str,
        cancellation: &CancellationToken,
    ) -> ContributionResult<rsi_agent_session_protocol::StepInputDecision> {
        match self {
            Self::Inline(decision) => Ok(decision),
            Self::Paged { bytes } => {
                if bytes == 0
                    || bytes > rsi_agent_session_protocol::MAXIMUM_PREPARED_INPUT_BYTES + 64
                {
                    return Err(invalid("invalid Step decision size"));
                }
                let mut data = String::with_capacity(bytes);
                while data.len() < bytes {
                    let value = adapter.connection.request("agent.step-decision.read", json!({
                        "session_id":session,"operation_id":operation,"token":token,"offset":data.len()
                    }), cancellation.clone()).await.map_err(invalid)?;
                    let part = value
                        .as_str()
                        .filter(|s| {
                            !s.is_empty() && s.len() <= (128 * 1024).min(bytes - data.len())
                        })
                        .ok_or_else(|| invalid("invalid Step decision fragment"))?;
                    data.push_str(part);
                }
                serde_json::from_str(&data).map_err(invalid)
            }
        }
    }
}

#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum StepReply {
    Staged(Box<StagedStep>),
    Rejected { diagnostic: String },
}

#[async_trait]
impl BeforeStepContributor for AgentAdapter {
    #[allow(clippy::too_many_lines)] // Retain exact external operation through validation, CAS staging and publication ownership transfer.
    async fn prepare(
        &self,
        context: &BeforeStepContext<'_>,
        cancellation: CancellationToken,
    ) -> ContributionResult<BeforeStepOutput> {
        let header = &context.captured.header;
        let session = header.session_id();
        let effect = EffectId::new(context.request_id.as_str()).map_err(invalid)?;
        let lane = self.staging_lane(session).map_err(invalid)?;
        let _staging = tokio::select! {
            () = cancellation.cancelled() => return Err(ContributionError::Closed),
            guard = lane.lock() => guard,
        };
        let captured = context
            .captured
            .domains
            .iter()
            .find(|view| {
                view.snapshot.identity().id()
                    == rsi_agent_session_protocol::EXTENSION_JOURNAL_DOMAIN
            })
            .ok_or_else(|| invalid("missing captured journal"))?;
        let (revision, journal) = self
            .load_current(header, Some(captured.revision), cancellation.clone())
            .await
            .map_err(invalid)?;
        self.unobserved
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(
                session.clone(),
                Unobserved {
                    turn: context.captured.turn_id.clone(),
                    effect: effect.clone(),
                },
            );
        let (previous, paged) = match context.previous {
            Some(decision) if decision.encoded_len() > 256 * 1024 => {
                let encoded = serde_json::to_string(decision).map_err(invalid)?;
                (json!({"bytes":encoded.len()}), Some(encoded))
            }
            other => (serde_json::to_value(other).map_err(invalid)?, None),
        };
        let _history = self
            .history
            .enter_step_reader(
                session.as_str(),
                effect.as_str(),
                context.captured.facts.clone(),
                context.captured.horizon.fact_seq,
                &cancellation,
                paged,
            )
            .map_err(invalid)?;
        let response = self.connection.request("agent.before-step", json!({
            "session_id": session, "operation_id": effect, "turn_id": context.captured.turn_id,
            "previous": previous, "model_selection": context.selection, "step_id": context.captured.step_id, "tools": context.tools, "fact_horizon": context.captured.horizon.fact_seq,
        }), cancellation.clone()).await.map_err(invalid)?;
        let mut staged = match serde_json::from_value::<StepReply>(response).map_err(invalid)? {
            StepReply::Staged(staged) => staged,
            StepReply::Rejected { diagnostic } => {
                if diagnostic.is_empty() || diagnostic.len() > 4096 {
                    return Err(invalid("invalid Step rejection"));
                }
                self.unobserved
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .remove(session);
                return Err(invalid(diagnostic));
            }
        };
        staged.tools.sort();
        if staged.operation_id != effect.as_str()
            || staged.expected_revision != revision.get()
            || staged.token.len() != 64
            || !staged.token.bytes().all(|byte| byte.is_ascii_hexdigit())
            || staged.tools.len() > context.tools.len()
            || staged.tools.windows(2).any(|pair| pair[0] == pair[1])
            || staged
                .tools
                .iter()
                .any(|name| !context.tools.contains(name))
            || staged.instructions.len()
                > rsi_agent_session_protocol::MAXIMUM_STEP_INSTRUCTION_BYTES
        {
            return Err(invalid(
                "staged Step identity, instructions or Tool subset mismatch",
            ));
        }
        let decision = staged
            .decision
            .resolve(
                self,
                session.as_str(),
                effect.as_str(),
                &staged.token,
                &cancellation,
            )
            .await?;
        let prepared = match crate::prepare(
            self.store.as_ref(),
            &self.handle,
            revision,
            &journal,
            staged.events,
            staged.checkpoint,
            self.read_budget.clone().into(),
        )
        .await
        {
            Ok(prepared) => prepared,
            Err(error) => {
                let response = self.connection.request("operation.receipt", json!({"sessionId":session,"operationId":effect,"token":staged.token,"outcome":"rejected","revision":revision.get()}), CancellationToken::new()).await.map_err(invalid)?;
                if !response.is_null() {
                    return Err(invalid("invalid rejection acknowledgement"));
                }
                self.unobserved
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .remove(session);
                return Err(invalid(error));
            }
        };
        let operation = Arc::new(Pending {
            connection: self.connection.clone(),
            session: session.clone(),
            effect,
            turn: context.captured.turn_id.clone(),
            token: staged.token,
            prepared,
            expected: Expected::Step {
                reject_input: decision.messages().is_none(),
                producer: context.producer.clone(),
                step: context.captured.step_id.clone(),
                instructions: staged.instructions.clone(),
                tools: staged.tools.clone(),
            },
            state: AtomicU8::new(0),
        });
        self.pending
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(session.clone(), operation.clone());
        self.unobserved
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(session);
        Ok(BeforeStepOutput {
            decision: Some(decision),
            instructions: staged.instructions,
            tools: Some(staged.tools),
            contribution: ContributionOutput {
                inputs: vec![],
                domains: vec![operation.prepared.proposal().clone()],
            },
            publications: vec![Publication::new(operation)],
        })
    }
}

pub(super) fn verify_facts(
    producer: &rsi_agent_session_protocol::ContributionId,
    step: &rsi_agent_session_protocol::StepId,
    instructions: &str,
    tools: &[String],
    facts: &[Arc<rsi_agent_session_protocol::SessionFact>],
) -> ContributionResult<()> {
    use rsi_agent_session_protocol::SessionFactBody;
    let mut prepared = 0;
    for fact in facts {
        if let SessionFactBody::StepPrepared {
            step_id,
            names,
            instructions: sections,
            ..
        } = fact.body()
        {
            if step_id != step || names.iter().any(|name| tools.binary_search(name).is_err()) {
                return Err(invalid("Step selection differs from staged authority"));
            }
            let texts = sections
                .iter()
                .filter(|section| &section.producer == producer)
                .map(|section| section.text.as_str());
            if !texts.eq((!instructions.is_empty()).then_some(instructions)) {
                return Err(invalid("Step instructions differ from staged text"));
            }
            prepared += 1;
        }
    }
    if prepared != 1 {
        return Err(invalid(
            "publication does not contain the complete staged Step",
        ));
    }
    Ok(())
}
