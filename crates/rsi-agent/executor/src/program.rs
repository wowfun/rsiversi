//! Bounded foreground dispatch while the coordinator still owns its exact claim.
use super::*;
use rsi_agent_session_protocol::{MAXIMUM_PROGRAM_OUTSTANDING_CALLS, ToolOrigin};
use rsi_agent_turn_protocol::{ProgramToolCalls, ProgramToolDispatcher};
use rsi_tools_protocol::{ToolDefinition, ToolProgramRole};
use tokio::sync::{mpsc, oneshot};

struct RequestInput {
    name: String,
    arguments: Value,
    cancellation: CancellationToken,
}
struct Request {
    input: RequestInput,
    reply: oneshot::Sender<rsi_tools_protocol::Result<ToolResult>>,
    _permit: OwnedSemaphorePermit,
}
#[derive(Debug)]
struct Dispatch {
    definitions: Vec<ToolDefinition>,
    sender: mpsc::Sender<Request>,
    capacity: Arc<Semaphore>,
}
#[async_trait]
impl ProgramToolDispatcher for Dispatch {
    fn definitions(&self) -> Vec<ToolDefinition> {
        self.definitions.clone()
    }
    async fn call(
        &self,
        name: String,
        arguments: Value,
        cancellation: CancellationToken,
    ) -> rsi_tools_protocol::Result<ToolResult> {
        if cancellation.is_cancelled() {
            return Err(ToolError::Cancelled);
        }
        if !self
            .definitions
            .iter()
            .any(|definition| definition.name() == name)
        {
            return Err(ToolError::Unknown(name));
        }
        let _permit = self
            .capacity
            .clone()
            .try_acquire_owned()
            .map_err(|_| ToolError::Capacity)?;
        let (reply, result) = oneshot::channel();
        self.sender
            .try_send(Request {
                input: RequestInput {
                    name,
                    arguments,
                    cancellation: cancellation.clone(),
                },
                reply,
                _permit,
            })
            .map_err(|error| match error {
                mpsc::error::TrySendError::Full(_) => ToolError::Capacity,
                mpsc::error::TrySendError::Closed(_) => ToolError::Cancelled,
            })?;
        // Executor owns the request after admission, including when this waiter disappears.
        tokio::select! { ()=cancellation.cancelled()=>Err(ToolError::Cancelled), result=result=>result.unwrap_or(Err(ToolError::Cancelled)) }
    }
}
impl Driver {
    #[allow(
        clippy::too_many_arguments,
        clippy::too_many_lines,
        reason = "The coordinator and nested calls share exact claim, policy, pin and cancellation owners."
    )]
    pub(super) async fn start_tool(
        &self,
        effect_id: &EffectId,
        prepared: Box<dyn PreparedToolCall>,
        identity: &ToolResultIdentity,
        composition: &AgentCompositionPin,
        claim: &TurnClaim,
        job_scope: Option<&JobScopeAuthority>,
        scheduling: ToolScheduling,
        turn_policy: ResolvedTurnPolicy,
        cancellation: &CancellationToken,
        stop: &CancellationToken,
        coordinator: Option<&mut ModelContextState>,
    ) -> std::result::Result<(ToolResult, Vec<Arc<SessionFact>>), DriveFailure> {
        let Some(fold) = coordinator else {
            return self
                .start_tool_raw(
                    effect_id,
                    prepared,
                    identity,
                    composition,
                    claim,
                    job_scope,
                    scheduling,
                    turn_policy.sandbox,
                    cancellation,
                    stop,
                    None,
                )
                .await
                .map(|result| (result, vec![]));
        };
        let (sender, mut receiver) = mpsc::channel(MAXIMUM_PROGRAM_OUTSTANDING_CALLS);
        let extension = ProgramToolCalls(Arc::new(Dispatch {
            definitions: composition
                .tools()
                .definitions()
                .into_iter()
                .filter(|definition| {
                    definition.program_role() == ToolProgramRole::Callable
                        && definition.scheduling() != ToolScheduling::ExclusiveFinal
                })
                .collect(),
            sender,
            capacity: Arc::new(Semaphore::new(MAXIMUM_PROGRAM_OUTSTANDING_CALLS)),
        }));
        let combined = combine_cancellation(cancellation, stop);
        let token = combined.token();
        let run = self.start_tool_raw(
            effect_id,
            prepared,
            identity,
            composition,
            claim,
            job_scope,
            scheduling,
            turn_policy.sandbox,
            &token,
            stop,
            Some(extension),
        );
        tokio::pin!(run);
        let mut ordinal = 1_u32;
        let mut settled = Vec::new();
        loop {
            let request = tokio::select! {
                result = &mut run => return result.map(|result| (result, settled)),
                request = receiver.recv() => match request {
                    Some(request) => request,
                    None => return run.await.map(|result| (result, settled)),
                },
            };
            if request.input.cancellation.is_cancelled() {
                let _ = request.reply.send(Err(ToolError::Cancelled));
                continue;
            }
            let nested = self.run_program_call(
                effect_id,
                ordinal,
                claim,
                composition,
                job_scope,
                fold,
                &request.input,
                turn_policy,
                &token,
                stop,
            );
            tokio::pin!(nested);
            let outcome = tokio::select! {
                result=&mut run=>{
                    // Completion is not cancellation: settle under the original Turn token.
                    // A completed script cannot abandon an admitted internal effect.
                    let (_, fact) = nested.await?;
                    settled.push(fact);
                    let _=request.reply.send(Err(ToolError::Cancelled));
                    return result.map(|result| (result, settled));
                }
                outcome=&mut nested=>outcome,
            };
            match outcome {
                Ok((result, fact)) => {
                    settled.push(fact);
                    let _ = request.reply.send(Ok(result));
                }
                Err(failure) => {
                    let _ = request.reply.send(Err(ToolError::Execution(
                        "program Tool execution failed; see its durable evidence".into(),
                    )));
                    receiver.close();
                    combined.cancel();
                    let _ = run.await;
                    return Err(failure);
                }
            }
            ordinal = ordinal
                .checked_add(1)
                .ok_or_else(|| fatal("program ordinal exhausted"))?;
        }
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "Nested admission keeps the original claim pin, policy, evidence fold and cancellation explicit."
    )]
    async fn run_program_call(
        &self,
        parent: &EffectId,
        ordinal: u32,
        claim: &TurnClaim,
        composition: &AgentCompositionPin,
        job_scope: Option<&JobScopeAuthority>,
        fold: &mut ModelContextState,
        request: &RequestInput,
        turn_policy: ResolvedTurnPolicy,
        cancellation: &CancellationToken,
        stop: &CancellationToken,
    ) -> std::result::Result<(ToolResult, Arc<SessionFact>), DriveFailure> {
        let definition = composition
            .tools()
            .definition(&request.name)
            .filter(|definition| {
                definition.program_role() == ToolProgramRole::Callable
                    && definition.scheduling() != ToolScheduling::ExclusiveFinal
            })
            .ok_or_else(|| {
                failed(
                    "program.ineligible",
                    "program Tool is not eligible in its frozen catalog",
                )
            })?;
        let combined = combine_cancellation(cancellation, &request.cancellation);
        let token = combined.token();
        let origin = ToolOrigin::Program {
            parent_effect_id: parent.clone(),
            ordinal,
        };
        let pending = self
            .prepare_tool_call(
                &origin,
                claim,
                composition,
                fold,
                ModelToolCall {
                    id: next_effect_id().map_err(fatal)?.to_string(),
                    name: request.name.clone(),
                    arguments: serde_json::to_string(&request.arguments).map_err(fatal)?,
                    kind: ToolCallKind::Function,
                },
                definition.scheduling(),
                turn_policy,
                &token,
                stop,
            )
            .await?;
        let prepared = self
            .publish_tool_start(claim, composition, fold, pending)
            .await?;
        let result = self
            .start_tool_raw(
                &prepared.effect_id,
                prepared.prepared,
                &prepared.identity,
                composition,
                claim,
                job_scope,
                definition.scheduling(),
                turn_policy.sandbox,
                &token,
                stop,
                None,
            )
            .await?;
        let fact = self
            .publish_tool_result(claim, composition, fold, prepared.intent, result)
            .await?;
        let SessionFactBody::ToolResult { result, .. } = fact.body() else {
            return Err(fatal("program settlement has no result"));
        };
        Ok((result.clone(), fact))
    }
}
