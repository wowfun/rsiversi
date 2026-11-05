//! Ordered callbacks run outside framework locks and submit one validated stage.

use super::*;
use crate::execution_support::turn_failure;
use rsi_agent_composition_protocol::{
    ContributionBatch, ContributionContext, ContributionKind, ContributionResult,
    ContributionStage, ToolPolicyDecision, ToolPolicyRequest,
};
use rsi_agent_session_protocol::{
    ContributionId, DomainRequestId, DomainStateView, ModelSelection, StepId, ToolRejection,
};

pub(super) const STAGE_DEADLINE: Duration = Duration::from_secs(30);

enum StepSnapshot {
    Callbacks(ContributionContext),
    Request(StepId),
}

impl StepSnapshot {
    fn step_id(&self) -> &StepId {
        match self {
            Self::Callbacks(context) => &context.step_id,
            Self::Request(id) => id,
        }
    }
}

struct StageRun<'a> {
    stage: ContributionStage,
    deadline: tokio::time::Instant,
    cancellation: &'a CancellationToken,
    stop: &'a CancellationToken,
    token: CancellationToken,
    _guard: tokio_util::sync::DropGuard,
}

impl<'a> StageRun<'a> {
    fn new(
        stage: ContributionStage,
        cancellation: &'a CancellationToken,
        stop: &'a CancellationToken,
    ) -> Self {
        let token = cancellation.child_token();
        let guard = token.clone().drop_guard();
        Self {
            stage,
            deadline: tokio::time::Instant::now() + STAGE_DEADLINE,
            cancellation,
            stop,
            token,
            _guard: guard,
        }
    }

    async fn capture<T>(
        &self,
        future: impl Future<Output = rsi_agent_turn_protocol::Result<T>>,
    ) -> std::result::Result<T, DriveFailure> {
        tokio::select! {
            () = self.stop.cancelled() => Err(DriveFailure::Stopped),
            () = self.cancellation.cancelled() => Err(DriveFailure::Turn(TurnOutcome::Cancelled)),
            result = tokio::time::timeout_at(self.deadline, future) => {
                result.map_err(|_| failed("contribution.timeout", format!("framework/{:?}: snapshot deadline", self.stage)))?
                    .map_err(turn_failure)
            }
        }
    }

    async fn before_step(
        turns: &dyn TurnExecution,
        claim: &TurnClaim,
        explicit: Option<&ModelSelection>,
        pending: Vec<rsi_agent_session_protocol::SettledToolContext>,
        has_callbacks: bool,
        cancellation: &'a CancellationToken,
        stop: &'a CancellationToken,
    ) -> std::result::Result<(ModelSelection, StepSnapshot, Self), DriveFailure> {
        let run = Self::new(ContributionStage::BeforeStep, cancellation, stop);
        let resolve = |domains: &[DomainStateView]| {
            rsi_agent_model_selection::resolve_selection(
                claim.header(),
                domains.iter().map(|view| &view.snapshot),
                explicit,
            )
        };
        let (selection, snapshot) = if has_callbacks {
            let context = run
                .capture(turns.contribution_context(claim, pending, run.token.clone()))
                .await?;
            (resolve(&context.domains), StepSnapshot::Callbacks(context))
        } else {
            let domain = explicit
                .is_none()
                .then_some(rsi_agent_model_selection::DOMAIN);
            let (id, state) = run
                .capture(turns.request_step(claim, domain, pending, run.token.clone()))
                .await?;
            (resolve(state.as_slice()), StepSnapshot::Request(id))
        };
        let selection = selection.map_err(|error| failed("model.selection", error.to_string()))?;
        Ok((selection, snapshot, run))
    }

    async fn callback<T>(
        &self,
        producer: &ContributionId,
        future: impl Future<Output = ContributionResult<T>>,
    ) -> std::result::Result<T, DriveFailure> {
        let label = format!("{producer}/{:?}", self.stage);
        tokio::select! {
            () = self.stop.cancelled() => Err(DriveFailure::Stopped),
            () = self.cancellation.cancelled() => Err(DriveFailure::Turn(TurnOutcome::Cancelled)),
            result = tokio::time::timeout_at(self.deadline, AssertUnwindSafe(future).catch_unwind()) => {
                result.map_err(|_| failed("contribution.timeout", format!("{label}: stage deadline")))?
                    .map_err(|_| failed("contribution.panic", format!("{label}: callback panicked")))?
                    .map_err(|error| failed("contribution.failed", format!("{label}: {}", bounded(&error.to_string()))))
            }
        }
    }
}

async fn prepare_final_inputs(
    composition: &AgentCompositionPin,
    context: &rsi_agent_composition_protocol::RequestInputContext<'_>,
    previous: Option<rsi_agent_session_protocol::StepInputDecision>,
    run: &StageRun<'_>,
) -> std::result::Result<rsi_agent_session_protocol::StepInputDecision, DriveFailure> {
    use rsi_agent_session_protocol::{InputClaimRef, PreparedInput, StepInputDecision};
    if previous
        .as_ref()
        .is_some_and(|decision| decision.messages().is_none())
    {
        return Ok(StepInputDecision::reject());
    }
    if let Some(preparer) = composition.request_input_preparer() {
        let context = rsi_agent_composition_protocol::RequestInputContext {
            previous: previous.as_ref(),
            ..*context
        };
        return tokio::select! { biased;
            () = run.stop.cancelled() => Err(DriveFailure::Stopped),
            () = run.cancellation.cancelled() => Err(DriveFailure::Turn(TurnOutcome::Cancelled)),
            result = tokio::time::timeout_at(run.deadline, AssertUnwindSafe(preparer.prepare(&context, run.token.clone())).catch_unwind()) =>
                result.map_err(|_| failed("input.timeout", "application preparation deadline"))?
                    .map_err(|_| failed("input.panic", "application preparation panicked"))?
                    .map_err(|error| failed("input.failed", bounded(&error.to_string()))),
        };
    }
    let mut messages = if let Some(previous) = previous {
        if context.contributed.is_empty() || previous.messages().is_some_and(<[_]>::is_empty) {
            return Ok(previous);
        }
        previous.into_messages().unwrap_or_default()
    } else {
        let mut messages = Vec::new();
        for fact in context.claims {
            for ordinal in (0_u32..).take(fact.claimed_input_count()) {
                messages.push(PreparedInput::Claimed {
                    claim: InputClaimRef::new(fact.seq(), ordinal).map_err(fatal)?,
                });
            }
        }
        messages
    };
    for offset in (1..=context.contributed.len()).rev() {
        messages.push(PreparedInput::Claimed {
            claim: InputClaimRef::preceding(offset + context.trailing_facts).map_err(fatal)?,
        });
    }
    StepInputDecision::enter(messages).map_err(fatal)
}

impl Driver {
    async fn capture_request_tools(
        &self,
        preset: &AgentCompositionPin,
        claim: &TurnClaim,
        source: &dyn rsi_agent_composition_protocol::RequestToolSource,
        input_required: bool,
        selection: &rsi_agent_session_protocol::ModelSelection,
        run: &StageRun<'_>,
    ) -> std::result::Result<AgentCompositionPin, DriveFailure> {
        let mut reserved = std::collections::BTreeSet::new();
        let assigned = source.replaced_preset_tools();
        let request_preset = || preset.clone().without_preset_tools(assigned.clone());
        let selected = request_preset();
        selected.tools().visit_definitions(&mut |tool| {
            reserved.insert(tool.name().to_owned());
        });
        let token = run.cancellation.child_token();
        let _guard = token.clone().drop_guard();
        let capture = tokio::time::timeout_at(
            run.deadline,
            source.capture(claim.header(), selection, &reserved, token),
        );
        let result = tokio::select! {biased;
            ()=run.stop.cancelled()=>return Err(DriveFailure::Stopped),
            ()=run.cancellation.cancelled()=>return Err(DriveFailure::Turn(TurnOutcome::Cancelled)),
            result=capture=>result,
        };
        let reason = match result {
            Ok(Ok(catalog)) if input_required && catalog.inputs.is_none() => {
                "application capture omitted its input preparer".into()
            }
            Ok(Ok(catalog)) => match selected.with_request_catalog(catalog, claim.header()) {
                Ok(pin) => return Ok(pin),
                Err(error) => error.to_string(),
            },
            Ok(Err(error)) => error.to_string(),
            Err(_) => "application tool capture timed out".into(),
        };
        if input_required {
            return Err(failed("application.input_unavailable", bounded(&reason)));
        }
        Ok(request_preset().with_unavailable_request_tools(&reason))
    }
    #[allow(clippy::too_many_arguments)] // The input decision shares the exact claim, fold and cancellation owners.
    pub(super) async fn before_step(
        &self,
        claim: &TurnClaim,
        composition: &AgentCompositionPin,
        fold: &mut ModelContextState,
        explicit: Option<&rsi_agent_session_protocol::ModelSelection>,
        needs_input: bool,
        cancellation: &CancellationToken,
        stop: &CancellationToken,
    ) -> std::result::Result<
        (
            AgentCompositionPin,
            rsi_agent_session_protocol::ModelSelection,
            Option<Vec<String>>,
        ),
        DriveFailure,
    > {
        let source = self
            .scope
            .lookup_local::<rsi_agent_composition_protocol::RequestToolSourceContract>();
        let input_required = if let Some(source) = &source {
            tokio::select! { biased;
                () = stop.cancelled() => return Err(DriveFailure::Stopped),
                () = cancellation.cancelled() => return Err(DriveFailure::Turn(TurnOutcome::Cancelled)),
                result = source.await_input_preparer(claim.header()) =>
                    result.map_err(|error| failed("application.input_unavailable", bounded(&error.to_string())))?,
            }
        } else {
            false
        };
        let has_callbacks = composition
            .contributions()
            .entries()
            .iter()
            .any(|entry| entry.stage() == ContributionStage::BeforeStep);
        let (selection, snapshot, run) = StageRun::before_step(
            self.turns.as_ref(),
            claim,
            explicit,
            fold.pending_tool_inputs(),
            has_callbacks,
            cancellation,
            stop,
        )
        .await?;
        let composition = if let Some(source) = source {
            self.capture_request_tools(
                composition,
                claim,
                source.as_ref(),
                input_required,
                &selection,
                &run,
            )
            .await?
        } else {
            composition.clone()
        };
        let (tools, empty) = self
            .run_captured_contributions(
                claim,
                &composition,
                fold,
                run,
                snapshot,
                &[],
                Some(&selection),
            )
            .await?;
        if needs_input && empty {
            return Err(DriveFailure::Turn(TurnOutcome::Completed));
        }
        Ok((composition, selection, tools))
    }

    #[allow(clippy::too_many_arguments)] // One stage shares the exact claim, fold and two cancellation owners.
    pub(super) async fn run_contributions(
        &self,
        claim: &TurnClaim,
        composition: &AgentCompositionPin,
        fold: &mut ModelContextState,
        stage: ContributionStage,
        settled: &[Arc<SessionFact>],
        cancellation: &CancellationToken,
        stop: &CancellationToken,
    ) -> std::result::Result<(), DriveFailure> {
        let entries = composition.contributions().entries();
        if !entries.iter().any(|entry| entry.stage() == stage) {
            return Ok(());
        }
        let run = StageRun::new(stage, cancellation, stop);
        let context = run
            .capture(
                self.turns
                    .contribution_context(claim, Vec::new(), run.token.clone()),
            )
            .await?;
        self.run_captured_contributions(
            claim,
            composition,
            fold,
            run,
            StepSnapshot::Callbacks(context),
            settled,
            None,
        )
        .await
        .map(|_| ())
    }

    #[allow(clippy::too_many_lines, clippy::too_many_arguments)] // Stage admission, atomic commit and observer notification share one captured owner.
    async fn run_captured_contributions(
        &self,
        claim: &TurnClaim,
        composition: &AgentCompositionPin,
        fold: &mut ModelContextState,
        run: StageRun<'_>,
        snapshot: StepSnapshot,
        settled: &[Arc<SessionFact>],
        selection: Option<&rsi_agent_session_protocol::ModelSelection>,
    ) -> std::result::Result<(Option<Vec<String>>, bool), DriveFailure> {
        let stage = run.stage;
        let entries = composition.contributions().entries();
        let prepares_tools = stage == ContributionStage::BeforeStep
            && entries
                .iter()
                .any(|entry| matches!(entry.kind(), ContributionKind::BeforeStep(_)));
        let record_tools = prepares_tools
            || stage == ContributionStage::BeforeStep && composition.request_tool_view().is_some();
        let mut offered = Vec::new();
        if record_tools {
            composition
                .tools()
                .visit_definitions(&mut |definition| offered.push(definition.name().to_owned()));
            offered.sort();
        }
        let mut mutation = None;
        let mut facts = Vec::new();
        let mut selected: Option<std::collections::BTreeSet<String>> = None;
        let mut publications = Vec::new();
        let mut instructions = if stage == ContributionStage::BeforeStep {
            composition
                .request_instruction()
                .cloned()
                .into_iter()
                .collect::<Vec<_>>()
        } else {
            Vec::new()
        };
        let mut previous: Option<rsi_agent_session_protocol::StepInputDecision> = None;
        if stage == ContributionStage::BeforeStep
            && !matches!(&snapshot, StepSnapshot::Callbacks(context) if context.horizon.fact_seq == fold.position().through_seq)
        {
            self.sync_fold(claim, fold).await?;
        }
        let claims = if stage == ContributionStage::BeforeStep {
            fold.claimed_inputs(claim.turn_id()).map_err(fatal)?
        } else {
            &[]
        };
        if let StepSnapshot::Callbacks(context) = &snapshot {
            let mut batch = ContributionBatch::default();
            let id =
                DomainRequestId::new(next_effect_id().map_err(fatal)?.as_str()).map_err(fatal)?;
            for entry in entries.iter().filter(|entry| entry.stage() == stage) {
                if previous
                    .as_ref()
                    .is_some_and(|decision| decision.messages().is_none())
                {
                    break;
                }
                let output = run
                    .callback(entry.id(), async {
                        match entry.kind() {
                            ContributionKind::BeforeStep(callback) => {
                                let output = callback
                                    .prepare(
                                        &rsi_agent_composition_protocol::BeforeStepContext {
                                            selection: selection.expect("BeforeStep selection"),
                                            producer: entry.id(),
                                            previous: previous.as_ref(),
                                            captured: context,
                                            request_id: &id,
                                            tools: &offered,
                                        },
                                        run.token.clone(),
                                    )
                                    .await?;
                                if publications.len().saturating_add(output.publications.len())
                                    > rsi_agent_composition_protocol::MAXIMUM_AGENT_CONTRIBUTIONS
                                {
                                    return Err(
                                        rsi_agent_composition_protocol::ContributionError::Capacity,
                                    );
                                }
                                publications.extend(output.publications);
                                if let Some(decision) = output.decision {
                                    if !decision.claims().all(|key| {
                                        let rsi_agent_session_protocol::InputClaimRef::Captured { fact_seq, ordinal } = key else { return false };
                                        claims.binary_search_by_key(&fact_seq.get(), SessionFact::seq)
                                            .is_ok_and(|i| (ordinal as usize) < claims[i].claimed_input_count())
                                    }) {
                                        return Err(rsi_agent_composition_protocol::ContributionError::Invalid(
                                            "preset decision references an input outside its capture".into()));
                                    }
                                    previous = Some(decision);
                                }
                                if !output.instructions.is_empty() {
                                    instructions.push(
                                        rsi_agent_session_protocol::StepInstruction {
                                            producer: entry.id().clone(),
                                            text: output.instructions,
                                        },
                                    );
                                    rsi_agent_session_protocol::validate_step_instructions(
                                        &instructions,
                                    )
                                    .map_err(|error| {
                                        rsi_agent_composition_protocol::ContributionError::Invalid(
                                            error.to_string(),
                                        )
                                    })?;
                                }
                                if let Some(names) = output.tools {
                                    if names.len() > offered.len()
                                        || names
                                            .iter()
                                            .any(|name| offered.binary_search(name).is_err())
                                    {
                                        return Err(
                                        rsi_agent_composition_protocol::ContributionError::Invalid(
                                            "Tool subset contains an unregistered name".into(),
                                        ),
                                    );
                                    }
                                    let count = names.len();
                                    let names: std::collections::BTreeSet<_> =
                                        names.into_iter().collect();
                                    if names.len() != count {
                                        return Err(
                                        rsi_agent_composition_protocol::ContributionError::Invalid(
                                            "duplicate Tool subset name".into(),
                                        ),
                                    );
                                    }
                                    selected = Some(match selected.take() {
                                        Some(previous) => {
                                            previous.intersection(&names).cloned().collect()
                                        }
                                        None => names,
                                    });
                                }
                                Ok(output.contribution)
                            }
                            ContributionKind::Context(callback) => {
                                callback.contribute(context, run.token.clone()).await
                            }
                            ContributionKind::PostTool(callback) => {
                                callback
                                    .contribute(context, settled, run.token.clone())
                                    .await
                            }
                            ContributionKind::ToolPolicy(_)
                            | ContributionKind::ForkTransform(_)
                            | ContributionKind::ToolSettlement(_)
                            | ContributionKind::Command(_)
                            | ContributionKind::Projection(_)
                            | ContributionKind::ResourceRead(_) => {
                                unreachable!("non-execution callbacks use their own dispatch")
                            }
                        }
                    })
                    .await;
                let output = match output {
                    Ok(output) => output,
                    Err(error) => {
                        super::tool_settlement::notify_publications(&mut publications, None)
                            .await?;
                        return Err(error);
                    }
                };
                if let Err(error) =
                    batch.append(entry.id(), claim.turn_id(), snapshot.step_id(), output)
                {
                    super::tool_settlement::notify_publications(&mut publications, None).await?;
                    return Err(failed(
                        "contribution.output",
                        format!("{}/{stage:?}: {error}", entry.id()),
                    ));
                }
            }
            let (output, proposals) = batch.into_parts();
            facts = output;
            if !proposals.is_empty() {
                mutation = Some((id, proposals));
            }
        }
        let selected = selected
            .map(|names| names.into_iter().collect::<Vec<_>>())
            .or_else(|| record_tools.then_some(offered));
        let mut reject_input = false;
        let mut empty = false;
        let decision = if stage == ContributionStage::BeforeStep {
            let context = rsi_agent_composition_protocol::RequestInputContext {
                turn_id: claim.turn_id(),
                step_id: snapshot.step_id(),
                fact_horizon: fold.position().through_seq,
                claims,
                contributed: &facts,
                trailing_facts: usize::from(selected.is_some()),
                previous: None,
            };
            let decision = match prepare_final_inputs(composition, &context, previous, &run).await {
                Ok(decision) => decision,
                Err(error) => {
                    super::tool_settlement::notify_publications(&mut publications, None).await?;
                    return Err(error);
                }
            };
            reject_input = decision.messages().is_none();
            empty = decision.messages().is_some_and(<[_]>::is_empty);
            Some(decision)
        } else {
            None
        };
        if let Some(names) = &selected {
            let application = composition.request_tool_view().cloned().map(|mut view| {
                if let rsi_agent_session_protocol::ApplicationToolView::Ready { tools, .. } =
                    &mut view
                {
                    tools.retain(|row| {
                        names
                            .binary_search_by(|name| name.as_str().cmp(row.definition.name()))
                            .is_ok()
                    });
                }
                view
            });
            facts.push(SessionFactBody::StepPrepared {
                turn_id: claim.turn_id().clone(),
                step_id: snapshot.step_id().clone(),
                names: names.clone(),
                application,
                instructions,
            });
        }
        if let Some(decision) = decision {
            facts.push(SessionFactBody::StepInputsPrepared {
                turn_id: claim.turn_id().clone(),
                step_id: snapshot.step_id().clone(),
                decision,
            });
        }
        if !publications.is_empty() && mutation.is_none() {
            return Err(failed(
                "contribution.output",
                "staged publication requires a domain proposal",
            ));
        }
        // Close captured readers before entering the mutation boundary. Never rerun callbacks
        // in response to a commit acknowledgement failure.
        drop((snapshot, run));
        if let Some((request_id, proposals)) = mutation {
            let expected_count = facts.len();
            let result = self
                .turns
                .commit_domains(
                    claim,
                    rsi_agent_turn_protocol::DomainMutation {
                        guards: vec![],
                        require_uncancelled_turn: stage == ContributionStage::BeforeStep,
                        request_id,
                        proposals,
                        facts,
                    },
                )
                .await;
            match result {
                Err(error) => {
                    if matches!(
                        error,
                        TurnError::DomainRevisionConflict { .. } | TurnError::Capacity
                    ) {
                        super::tool_settlement::notify_publications(&mut publications, None)
                            .await?;
                    }
                    if !(stage == ContributionStage::AfterTools
                        && matches!(error, TurnError::DomainRevisionConflict { .. }))
                    {
                        return Err(turn_failure(error));
                    }
                }
                Ok(receipt) => {
                    if !publications.is_empty() {
                        let mut published = Vec::new();
                        if let Some(span) = receipt.commit().fact_span() {
                            if span.count() != u64::try_from(expected_count).map_err(fatal)? {
                                return Err(fatal("contribution receipt Fact count mismatch"));
                            }
                            let mut cursor = span.first_seq() - 1;
                            while published.len() < expected_count {
                                let previous = cursor;
                                let page = self
                                    .turns
                                    .read_facts(
                                        claim,
                                        cursor,
                                        (expected_count - published.len()).min(256),
                                    )
                                    .await
                                    .map_err(fatal)?;
                                for fact in page.facts {
                                    if fact.seq() != cursor + 1 {
                                        return Err(fatal(
                                            "contribution receipt Fact sequence mismatch",
                                        ));
                                    }
                                    cursor = fact.seq();
                                    published.push(fact);
                                }
                                if published.len() > expected_count {
                                    return Err(fatal("contribution receipt crossed its span"));
                                }
                                if cursor == previous {
                                    return Err(fatal("contribution receipt unavailable"));
                                }
                            }
                        } else if expected_count != 0 {
                            return Err(fatal("contribution receipt lacks its Facts"));
                        }
                        let evidence = rsi_agent_composition_protocol::PublicationReceipt {
                            session_id: claim.session_id(),
                            facts: &published,
                            domain_commit: Some((receipt.control_seq(), receipt.commit())),
                        };
                        if let Err(error) = super::tool_settlement::notify_publications(
                            &mut publications,
                            Some(&evidence),
                        )
                        .await
                        {
                            if !reject_input {
                                return Err(error);
                            }
                            // The rejection is durable. The observer retains its
                            // unconfirmed operation for receipt reconciliation.
                            eprintln!(
                                "committed input rejection for {}: {error:?}",
                                claim.turn_id()
                            );
                        }
                    }
                }
            }
            self.sync_fold(claim, fold).await?;
        } else if !facts.is_empty() {
            self.publish_durable(claim, fold, facts).await?;
        } else {
            self.sync_fold(claim, fold).await?;
        }
        if reject_input {
            return Err(DriveFailure::Turn(TurnOutcome::Blocked));
        }
        Ok((selected, empty))
    }

    pub(super) async fn tool_policy_decision(
        &self,
        claim: &TurnClaim,
        composition: &AgentCompositionPin,
        mut request: ToolPolicyRequest<'_>,
        origin: &rsi_agent_session_protocol::ToolOrigin,
        cancellation: &CancellationToken,
        stop: &CancellationToken,
    ) -> std::result::Result<(bool, Option<ToolRejection>), DriveFailure> {
        let stage = ContributionStage::ToolPolicy;
        let entries = composition.contributions().entries();
        if !entries.iter().any(|entry| entry.stage() == stage) {
            return Ok((request.require_approval, None));
        }
        let program = match origin {
            rsi_agent_session_protocol::ToolOrigin::Program {
                parent_effect_id, ..
            } => Some(parent_effect_id),
            rsi_agent_session_protocol::ToolOrigin::Model { .. } => None,
        };
        let run = StageRun::new(stage, cancellation, stop);
        let context = run
            .capture(async {
                match program {
                    Some(parent) => {
                        self.turns
                            .program_policy_context(claim, parent, run.token.clone())
                            .await
                    }
                    None => {
                        self.turns
                            .contribution_context(claim, Vec::new(), run.token.clone())
                            .await
                    }
                }
            })
            .await?;
        for entry in entries {
            let ContributionKind::ToolPolicy(callback) = entry.kind() else {
                continue;
            };
            let decision = run
                .callback(entry.id(), async {
                    callback.decide(&context, &request, run.token.clone()).await
                })
                .await?;
            match decision {
                ToolPolicyDecision::Abstain => {}
                ToolPolicyDecision::RequireApproval => request.require_approval = true,
                ToolPolicyDecision::Deny { reason } => {
                    let rejection = ToolRejection::PolicyDenied {
                        contribution_id: entry.id().clone(),
                        reason,
                    };
                    rejection.validate().map_err(|error| {
                        failed(
                            "contribution.output",
                            format!("{}/{stage:?}: {error}", entry.id()),
                        )
                    })?;
                    return Ok((request.require_approval, Some(rejection)));
                }
            }
        }
        Ok((request.require_approval, None))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(start_paused = true)]
    async fn callback_free_steps_read_only_an_implicit_selection() {
        let (claim, _) = crate::tests::claim();
        let explicit = ModelSelection::baseline(claim.header().settings());
        let turns = crate::tests::pending_context(Arc::new(Mutex::new(Vec::new())));
        let cancellation = CancellationToken::new();
        let stop = CancellationToken::new();
        for (override_model, step) in [(Some(&explicit), "explicit"), (None, "selected")] {
            let Ok((selection, snapshot, _)) = StageRun::before_step(
                turns.as_ref(),
                &claim,
                override_model,
                Vec::new(),
                false,
                &cancellation,
                &stop,
            )
            .await
            else {
                panic!("callback-free Step captured a full context")
            };
            assert_eq!(snapshot.step_id().as_str(), step);
            if override_model.is_some() {
                assert_eq!(selection, explicit);
            } else {
                assert_eq!(
                    selection.model,
                    ModelRef::new("fixture", "persisted").unwrap()
                );
            }
        }
    }

    #[tokio::test(start_paused = true)]
    async fn input_capture_is_bounded_even_with_an_explicit_model_override() {
        let (claim, _) = crate::tests::claim();
        let explicit =
            rsi_agent_session_protocol::ModelSelection::baseline(claim.header().settings());
        let tokens = Arc::new(Mutex::new(Vec::new()));
        let turns = crate::tests::pending_context(tokens.clone());
        for mode in 0..4 {
            let cancellation = CancellationToken::new();
            let stop = CancellationToken::new();
            let mut pending = Box::pin(StageRun::before_step(
                turns.as_ref(),
                &claim,
                (mode != 0).then_some(&explicit),
                Vec::new(),
                true,
                &cancellation,
                &stop,
            ));
            assert!(futures_util::poll!(pending.as_mut()).is_pending());
            assert_eq!(tokens.lock().unwrap().len(), mode + 1);
            match mode {
                0 | 1 => tokio::time::advance(Duration::from_secs(31)).await,
                2 => cancellation.cancel(),
                _ => stop.cancel(),
            }
            let result = pending.await;
            assert!(match (mode, result) {
                (0 | 1, Err(DriveFailure::Turn(TurnOutcome::Failed { code, .. }))) =>
                    code == "contribution.timeout",
                (2, Err(DriveFailure::Turn(TurnOutcome::Cancelled)))
                | (3, Err(DriveFailure::Stopped)) => true,
                _ => false,
            });
            assert!(tokens.lock().unwrap().last().unwrap().is_cancelled());
        }
    }
}
