use super::*;
use rsi_tools_protocol::{ToolDecision, ToolGate};

impl Driver {
    pub(super) async fn run_claim(
        &self,
        claim: TurnClaim,
        stop: &CancellationToken,
        observation_slot: Option<OwnedSemaphorePermit>,
    ) {
        let (tracker, observation) = controlled_work::Tracker::new();
        let guard = tracker.guard();
        if self
            .turns
            .publish_controlled_work(&claim, observation.clone())
            .is_err()
        {
            // A predecessor may still be settling. No execution was admitted and
            // a registry conflict is not a durable failure of the Turn.
            tokio::select! { biased;
                () = stop.cancelled() => {},
                () = tokio::time::sleep(Duration::from_millis(25)) => {},
            }
            let _ignored = self.turns.release(&claim);
            return;
        }
        let key = (claim.session_id().clone(), claim.turn_id().clone());
        self.controlled_work
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(key.clone(), tracker);
        let deadline = tokio::time::Instant::now() + observation::OBSERVATION_WAIT;
        let preparation_stop = stop.child_token();
        let preparation_guard = preparation_stop.clone().drop_guard();
        let recovered = self.observer.is_some()
            && Self::observation_recovered(self.turns.as_ref(), &claim, stop, deadline).await;
        let start = rsi_agent_turn_protocol::ExecutionObservationStart {
            header: claim.header().clone(),
            execution: claim.execution().cloned(),
            turn: claim.turn_id().clone(),
            claim: claim.claim_id(),
            accepted_seq: claim.accepted_seq(),
            live_seq: claim.live_seq(),
            recovered,
        };
        let admission = if let Some(observer) = &self.observer {
            tokio::select! { biased;
                () = stop.cancelled() => Err("observation admission cancelled".to_owned()),
                result = tokio::time::timeout_at(deadline, observer.observe(start, preparation_stop.clone())) =>
                    result.unwrap_or_else(|_| Err("observation admission deadline elapsed".into())).map(|admission| {
                        match admission {
                            rsi_agent_turn_protocol::ExecutionObservationAdmission::Interval(interval) => Some(interval),
                            rsi_agent_turn_protocol::ExecutionObservationAdmission::OmittedCapacity => {
                                observation::report_capacity_omission(&claim);
                                None
                            }
                        }
                    }),
            }
        } else {
            Ok(None)
        };
        let interval = match admission {
            Ok(interval) => interval,
            Err(error) => {
                preparation_stop.cancel();
                let _ = publish_terminal(
                    &*self.turns,
                    &self.config,
                    &claim,
                    failure_outcome("observation.admission", bounded(&error)),
                )
                .await;
                self.controlled_work
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .remove(&key);
                drop(guard);
                let _ignored = self.turns.release(&claim);
                return;
            }
        };
        let began = match &interval {
            Some(interval) => observation::begin(interval, stop, deadline).await,
            None => true,
        };
        drop(preparation_guard);
        self.run_observed_claim(claim.clone(), stop).await;
        // Only observation ownership is dropped; Tools retains uncertain effects.
        let unconfirmed = self
            .active_tools
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&key);
        drop(unconfirmed);
        self.controlled_work
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&key);
        guard.confirm();
        let _ignored = self.turns.release(&claim);
        if let Some(interval) = interval {
            self.observations.finish(
                interval,
                began,
                observation,
                stop.clone(),
                observation_slot.expect("observed claims reserve an interval slot"),
            );
        }
    }

    pub(super) async fn observation_recovered(
        turns: &dyn TurnExecution,
        claim: &TurnClaim,
        stop: &CancellationToken,
        deadline: tokio::time::Instant,
    ) -> bool {
        let mut after = claim.accepted_seq();
        let mut count = 0;
        while after < claim.live_seq() {
            let reading =
                tokio::time::timeout_at(deadline, turns.read_facts(claim, after, 3 - count));
            let page = tokio::select! { biased;
                () = stop.cancelled() => None,
                result = reading => result.ok().and_then(std::result::Result::ok),
            };
            let Some(page) = page else { return true };
            count += page.facts.len();
            if count > 2
                || page.through_seq <= after
                || page.facts.iter().any(|fact| {
                    !matches!(
                        fact.body(),
                        SessionFactBody::StepStarted { .. }
                            | SessionFactBody::InputMessageClaimed { .. }
                    )
                })
            {
                return true;
            }
            after = page.through_seq;
        }
        false
    }

    async fn run_observed_claim(&self, claim: TurnClaim, stop: &CancellationToken) {
        let Some((job_scope, _job_status)) = self.prepare_jobs(&claim).await else {
            return;
        };
        let job_scope = Some(job_scope);
        let composition = match self.turns.composition(&claim) {
            Ok(composition) => composition,
            Err(error) => {
                self.finish_context_error(&claim, job_scope.as_ref(), error.to_string())
                    .await;
                return;
            }
        };
        let claim_stop = stop.child_token();
        let limits = self.context_limits();
        let mut fold = match ModelContextState::open(
            composition.context_builder(),
            claim.header().clone(),
            limits,
            self.context_budget.clone(),
        ) {
            Ok(fold) => fold,
            Err(error) => {
                let _ignored = self
                    .finish(
                        &claim,
                        job_scope.as_ref(),
                        failure_outcome(
                            if matches!(error, rsi_agent_context::ContextError::Capacity) {
                                "context.capacity"
                            } else {
                                "context.invalid"
                            },
                            error.to_string(),
                        ),
                    )
                    .await;
                return;
            }
        };
        let drive = select_drive_or_stop(
            &claim_stop,
            self.drive(
                &claim,
                &composition,
                job_scope.as_ref(),
                &claim_stop,
                &mut fold,
            ),
        )
        .await;
        self.settle_drive(&claim, &composition, job_scope.as_ref(), drive, stop)
            .await;
    }

    pub(super) async fn claim_next(
        &self,
        stop: &CancellationToken,
    ) -> std::result::Result<Option<TurnClaim>, TurnError> {
        self.reap_retirement_tasks();
        self.turns
            .claim(&self.config.executor_id, stop.clone())
            .await
    }

    pub(super) fn acquire_job_scope(
        &self,
        claim: &TurnClaim,
    ) -> std::result::Result<JobScopeAuthority, String> {
        let id = JobScopeId::new(
            "rsi.agent.turn",
            [claim.session_id().as_str(), claim.turn_id().as_str()],
        )
        .map_err(|error| error.to_string())?;
        self.jobs
            .acquire_scope(id)
            .map_err(|error| error.to_string())
    }

    pub(super) async fn settle_drive(
        &self,
        claim: &TurnClaim,
        composition: &AgentCompositionPin,
        job_scope: Option<&JobScopeAuthority>,
        drive: std::result::Result<(), DriveFailure>,
        stop: &CancellationToken,
    ) {
        let (outcome, settled_identity) = match drive {
            Ok(()) | Err(DriveFailure::Stopped) => return,
            Err(DriveFailure::Turn(outcome)) => (outcome, None),
            Err(DriveFailure::SettledTool { outcome, identity }) => (outcome, Some(identity)),
            Err(DriveFailure::Fatal(message)) => (
                TurnOutcome::Failed {
                    code: "executor.internal".into(),
                    message,
                },
                None,
            ),
        };
        if self.finish(claim, job_scope, outcome).await.is_ok() {
            self.request_checkpoint(claim, composition);
            if let Some(identity) = settled_identity {
                let runtime = self
                    .active_tools
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .get(&(claim.session_id().clone(), claim.turn_id().clone()))
                    .and_then(|calls| calls.get(&identity))
                    .map_or_else(
                        || composition.tools(),
                        |tracked| tracked.composition.tools(),
                    );
                let _ignored = runtime.commit(&identity);
                self.clear_tracked_tool(claim, &identity);
            }
            self.retire_tracked_tools(claim, stop);
        }
    }

    pub(super) async fn drive(
        &self,
        claim: &TurnClaim,
        composition: &AgentCompositionPin,
        job_scope: Option<&JobScopeAuthority>,
        stop: &CancellationToken,
        fold: &mut ModelContextState,
    ) -> std::result::Result<(), DriveFailure> {
        let state = self.load_claim(claim, fold).await?;
        if state.terminal {
            return Ok(());
        }
        if state.concluded {
            return Err(DriveFailure::Turn(TurnOutcome::Completed));
        }
        if state.completed_model_without_successor {
            return Err(DriveFailure::Turn(TurnOutcome::Interrupted {
                effect: Some(EffectKind::Model),
                reason: "a completed model effect lacks a durable terminal or successor intent"
                    .into(),
            }));
        }
        self.resume_effects(claim, composition, fold, state.effects, stop)
            .await?;

        if let Some((model, request)) = state.image {
            return self.run_image(claim, fold, model, request, stop).await;
        }

        let turn_policy = state.turn_policy.ok_or_else(|| {
            failed(
                "executor.invalid_history",
                "Language turn lacks a resolved execution policy",
            )
        })?;
        let model = state
            .model
            .map(|model| rsi_agent_session_protocol::ModelSelection {
                model,
                reasoning_effort: state.reasoning_effort,
            });
        self.run_language(
            claim,
            composition,
            job_scope,
            fold,
            model,
            turn_policy,
            state.conversation_source.is_none(),
            stop,
        )
        .await
    }

    #[allow(clippy::too_many_arguments, clippy::too_many_lines)] // Keep one visible model/Tool durability loop over the exact claim authorities.
    pub(super) async fn run_language(
        &self,
        claim: &TurnClaim,
        composition: &AgentCompositionPin,
        job_scope: Option<&JobScopeAuthority>,
        fold: &mut ModelContextState,
        explicit_selection: Option<rsi_agent_session_protocol::ModelSelection>,
        turn_policy: ResolvedTurnPolicy,
        mut initial_request: bool,
        stop: &CancellationToken,
    ) -> std::result::Result<(), DriveFailure> {
        let mut retry_attempt = 0_u8;
        let mut selection =
            rsi_agent_session_protocol::ModelSelection::baseline(claim.header().settings());
        let mut selected_tools = None;
        let mut request_composition = composition.clone();
        loop {
            if stop.is_cancelled() {
                return Err(DriveFailure::Stopped);
            }
            let cancellation = self.turns.cancellation(claim).map_err(fatal)?;
            if cancellation.is_cancelled() {
                return Err(DriveFailure::Turn(TurnOutcome::Cancelled));
            }
            if retry_attempt == 0
                && self
                    .turns
                    .claim_pending_step_messages(claim)
                    .await
                    .map_err(fatal)?
                    > 0
            {
                self.sync_fold(claim, fold).await?;
            }
            if retry_attempt == 0 {
                (request_composition, selection, selected_tools) = self
                    .before_step(
                        claim,
                        composition,
                        fold,
                        explicit_selection.as_ref(),
                        initial_request,
                        &cancellation,
                        stop,
                    )
                    .await?;
            }
            let composition = &request_composition;
            initial_request = false;
            let (source_model_effect_id, output) = match self
                .run_model_attempt(
                    claim,
                    composition,
                    fold,
                    &selection,
                    selected_tools.as_deref(),
                    retry_attempt,
                    &cancellation,
                    stop,
                )
                .await?
            {
                ModelAttempt::ContextLimit => {
                    return Err(failed(
                        "context.limit",
                        "ordinary request exceeded context after recovery",
                    ));
                }
                ModelAttempt::Retry => {
                    retry_attempt = retry_attempt.saturating_add(1);
                    continue;
                }
                ModelAttempt::Incomplete => return Err(DriveFailure::Turn(TurnOutcome::MaxTokens)),
                ModelAttempt::Output(effect_id, output) => (effect_id, *output),
            };
            if cancellation.is_cancelled()
                || matches!(output.finish_reason, FinishReason::Cancelled)
            {
                return Err(DriveFailure::Turn(TurnOutcome::Cancelled));
            }
            if self
                .turns
                .claim_pending_step_messages(claim)
                .await
                .map_err(fatal)?
                > 0
            {
                self.sync_fold(claim, fold).await?;
                retry_attempt = 0;
                continue;
            }
            if !matches!(output.finish_reason, FinishReason::ToolCalls) {
                return Err(DriveFailure::Turn(TurnOutcome::Completed));
            }
            retry_attempt = 0;
            let calls = output
                .content
                .into_iter()
                .filter_map(|content| match content {
                    rsi_ai_protocol::ContentBlock::ToolCall(call) => Some(call),
                    rsi_ai_protocol::ContentBlock::Text { .. }
                    | rsi_ai_protocol::ContentBlock::Reasoning { .. } => None,
                })
                .collect::<Vec<_>>();
            if calls.is_empty() {
                return Err(failed(
                    "provider.tool_calls_missing",
                    "Tool-call finish reason contained no Tool call",
                ));
            }
            let tools = composition.tools();
            let mut scheduled = VecDeque::with_capacity(calls.len());
            let total_calls = calls.len();
            for (index, call) in calls.into_iter().enumerate() {
                if selected_tools
                    .as_ref()
                    .is_some_and(|names| names.binary_search(&call.name).is_err())
                {
                    return Err(failed(
                        "tool.not_admitted",
                        format!("Tool '{}' was not selected for this Step", call.name),
                    ));
                }
                let scheduling = tools.scheduling(&call.name).ok_or_else(|| {
                    failed(
                        "tool.not_found",
                        format!("Tool '{}' is absent from the sealed catalog", call.name),
                    )
                })?;
                if scheduling == ToolScheduling::ExclusiveFinal && index + 1 != total_calls {
                    return Err(failed(
                        "tool.exclusive_final_not_last",
                        format!(
                            "Tool '{}' must be the last call in provider source order",
                            call.name
                        ),
                    ));
                }
                scheduled.push_back((call, scheduling));
            }
            let mut settled = Vec::new();
            while let Some((call, scheduling)) = scheduled.pop_front() {
                match scheduling {
                    ToolScheduling::Exclusive | ToolScheduling::ExclusiveFinal => {
                        settled.push(
                            self.run_tool(
                                &source_model_effect_id,
                                claim,
                                composition,
                                job_scope,
                                fold,
                                call,
                                scheduling,
                                turn_policy,
                                &cancellation,
                                stop,
                            )
                            .await?,
                        );
                    }
                    ToolScheduling::ParallelSafe => {
                        let mut batch = vec![call];
                        while scheduled
                            .front()
                            .is_some_and(|(_, next)| *next == ToolScheduling::ParallelSafe)
                        {
                            let (call, _) = scheduled
                                .pop_front()
                                .expect("front was a parallel-safe Tool call");
                            batch.push(call);
                        }
                        settled.extend(
                            self.run_parallel_tools(
                                &source_model_effect_id,
                                claim,
                                composition,
                                job_scope,
                                fold,
                                batch,
                                turn_policy,
                                &cancellation,
                                stop,
                            )
                            .await?,
                        );
                    }
                }
            }
            self.run_contributions(
                claim,
                composition,
                fold,
                rsi_agent_composition_protocol::ContributionStage::AfterTools,
                &settled,
                &cancellation,
                stop,
            )
            .await?;
        }
    }

    pub(super) async fn resume_effects(
        &self,
        claim: &TurnClaim,
        composition: &AgentCompositionPin,
        fold: &mut ModelContextState,
        effects: Vec<ResumeEffect>,
        stop: &CancellationToken,
    ) -> std::result::Result<(), DriveFailure> {
        for effect in effects {
            match effect {
                ResumeEffect::Model { started, .. } => {
                    return Err(DriveFailure::Turn(TurnOutcome::Interrupted {
                        effect: started.then_some(EffectKind::Model),
                        reason: "executor generation changed during a prepared model effect".into(),
                    }));
                }
                ResumeEffect::Image { started, .. } => {
                    return Err(DriveFailure::Turn(TurnOutcome::Interrupted {
                        effect: started.then_some(EffectKind::Image),
                        reason: "executor generation changed during a prepared Image effect".into(),
                    }));
                }
                ResumeEffect::Tool { started: false, .. } => {
                    return Err(DriveFailure::Turn(TurnOutcome::Interrupted {
                        effect: None,
                        reason: "executor generation changed before a prepared Tool started".into(),
                    }));
                }
                ResumeEffect::Tool {
                    intent,
                    started: true,
                    ..
                } => {
                    self.recover_tool(claim, composition, fold, intent, stop)
                        .await?;
                }
            }
        }
        Ok(())
    }

    pub(super) async fn run_image(
        &self,
        claim: &TurnClaim,
        fold: &mut ModelContextState,
        model: ModelRef,
        request: ImageRequest,
        stop: &CancellationToken,
    ) -> std::result::Result<(), DriveFailure> {
        if stop.is_cancelled() {
            return Err(DriveFailure::Stopped);
        }
        let cancellation = self.turns.cancellation(claim).map_err(fatal)?;
        if cancellation.is_cancelled() {
            return Err(DriveFailure::Turn(TurnOutcome::Cancelled));
        }
        let _execution = admit_claim_execution(claim).map_err(|failure| *failure)?;
        let expected_outputs = usize::from(request.count());
        let prepared = tokio::select! {
            biased;
            () = stop.cancelled() => return Err(DriveFailure::Stopped),
            () = cancellation.cancelled() => return Err(DriveFailure::Turn(TurnOutcome::Cancelled)),
            result = self.image.prepare(model, request) =>
                result.map_err(|error| image_ai_failure(&error, Vec::new()))?,
        };
        if stop.is_cancelled() {
            return Err(DriveFailure::Stopped);
        }
        if cancellation.is_cancelled() {
            return Err(DriveFailure::Turn(TurnOutcome::Cancelled));
        }
        let snapshot = prepared.snapshot().clone();
        let effect_id = next_effect_id().map_err(fatal)?;
        self.publish_durable(
            claim,
            fold,
            vec![SessionFactBody::ImageIntent {
                turn_id: claim.turn_id().clone(),
                effect_id: effect_id.clone(),
                snapshot,
            }],
        )
        .await?;

        self.publish_durable(
            claim,
            fold,
            vec![SessionFactBody::ImageStarted {
                turn_id: claim.turn_id().clone(),
                effect_id: effect_id.clone(),
            }],
        )
        .await?;

        let combined = combine_cancellation(&cancellation, stop);
        let stream = match prepared.start(combined.token()).await {
            Ok(stream) => stream,
            Err(error) => {
                combined.cancel();
                if stop.is_cancelled() {
                    return Err(DriveFailure::Stopped);
                }
                return Err(image_ai_failure(&error, Vec::new()));
            }
        };
        self.consume_image_stream(
            fold,
            ImageStreamContext {
                claim,
                effect_id: &effect_id,
                expected_outputs,
                stop,
                combined,
            },
            stream,
        )
        .await
    }

    pub(super) async fn consume_image_stream(
        &self,
        fold: &mut ModelContextState,
        attempt: ImageStreamContext<'_>,
        mut stream: rsi_ai_protocol::ImageStream,
    ) -> std::result::Result<(), DriveFailure> {
        let ImageStreamContext {
            claim,
            effect_id,
            expected_outputs,
            stop,
            combined,
        } = attempt;
        let mut assembler = ImageAssembler::new();
        let mut media = Vec::with_capacity(expected_outputs);
        loop {
            let event = match stream.next().await {
                Some(Ok(event)) => event,
                Some(Err(error)) => {
                    combined.cancel();
                    if stop.is_cancelled() {
                        return Err(DriveFailure::Stopped);
                    }
                    return Err(image_ai_failure(&error, media));
                }
                None => {
                    combined.cancel();
                    return Err(image_operation_failure(
                        media,
                        "provider.missing_terminal",
                        "Image stream ended without a terminal event",
                    ));
                }
            };
            let completed_index = match &event {
                ImageEvent::OutputFinished { index } => Some(*index),
                ImageEvent::OutputStarted { .. }
                | ImageEvent::OutputChunk { .. }
                | ImageEvent::Usage { .. }
                | ImageEvent::Finished => None,
            };
            assembler.push(&event).map_err(|error| {
                image_operation_failure(media.clone(), error.code(), error.to_string())
            })?;
            if let Some(index) = completed_index {
                let output = assembler.take_completed(index).ok_or_else(|| {
                    image_operation_failure(
                        media.clone(),
                        "stream.missing_output",
                        "closed Image output was not retained",
                    )
                })?;
                let reference =
                    self.media
                        .import_image(output.bytes.into())
                        .await
                        .map_err(|error| {
                            image_operation_failure(
                                media.clone(),
                                "media.commit",
                                error.to_string(),
                            )
                        })?;
                self.publish_durable(
                    claim,
                    fold,
                    vec![SessionFactBody::ImageOutput {
                        turn_id: claim.turn_id().clone(),
                        effect_id: effect_id.clone(),
                        index,
                        media: reference.clone(),
                    }],
                )
                .await?;

                media.push(reference);
            }
            if matches!(event, ImageEvent::Finished) {
                combined.cancel();
                let completed_outputs = assembler.completed_count();
                let _output = assembler.finish().map_err(|error| {
                    image_operation_failure(media.clone(), error.code(), error.to_string())
                })?;
                if completed_outputs != expected_outputs || media.len() != expected_outputs {
                    return Err(image_operation_failure(
                        media,
                        "provider.output_count",
                        "Image provider returned a different number of outputs than requested",
                    ));
                }
                return Err(DriveFailure::Turn(TurnOutcome::Completed));
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    // One attempt binds the resident generation to its durable fold, model, retry ordinal, and cancellation fences.
    #[allow(clippy::too_many_lines)] // Prepared settings, optional evidence fallback and dispatch share one effect transaction.
    pub(super) async fn run_model_effect(
        &self,
        claim: &TurnClaim,
        request: rsi_ai_protocol::LanguageRequest,
        purpose: rsi_agent_session_protocol::ModelPurpose,
        fold: &mut ModelContextState,
        selection: &rsi_agent_session_protocol::ModelSelection,
        profile: &rsi_ai_protocol::LanguageProfile,
        retry_attempt: u8,
        cancellation: &CancellationToken,
        stop: &CancellationToken,
    ) -> std::result::Result<ModelAttempt, DriveFailure> {
        if request.settings().reasoning_effort() != selection.reasoning_effort.as_ref() {
            return Err(failed(
                "context.settings",
                "context builder changed the selected reasoning effort",
            ));
        }
        let expected_cap = match &purpose {
            rsi_agent_session_protocol::ModelPurpose::Conversation => claim
                .header()
                .settings()
                .output_budgets()
                .cap(&selection.model)
                .or(profile.default_output_tokens()),
            rsi_agent_session_protocol::ModelPurpose::ContextCompaction(plan) => {
                Some(plan.maximum_output_tokens)
            }
        };
        if request.settings().max_output_tokens() != expected_cap {
            return Err(failed(
                "context.settings",
                "context builder changed the selected output cap",
            ));
        }
        let _execution = admit_claim_execution(claim).map_err(|failure| *failure)?;
        let capture = evidence::Capture::new(&request, profile)?;
        let prepared_result = tokio::select! { biased;
            () = stop.cancelled() => return Err(DriveFailure::Stopped),
            () = cancellation.cancelled() => return Err(DriveFailure::Turn(TurnOutcome::Cancelled)),
            result = self.language.prepare(selection.model.clone(), request) => result,
        };
        let prepared = match prepared_result {
            Ok(prepared) => prepared,
            Err(error)
                if error.kind() == ErrorKind::ContextLimit
                    && error.dispatch_status() != DispatchStatus::Unknown =>
            {
                return Ok(ModelAttempt::ContextLimit);
            }
            Err(error) => return Err(ai_failure(&error)),
        };
        let snapshot = prepared.snapshot().clone();
        let effect_id = next_effect_id().map_err(fatal)?;
        let reservation_cancel = combine_cancellation(cancellation, stop);
        let mut live = self
            .turns
            .reserve_model_attempt(
                claim,
                effect_id.clone(),
                purpose.event_purpose(),
                reservation_cancel.token(),
            )
            .await
            .map_err(|error| {
                if stop.is_cancelled() {
                    DriveFailure::Stopped
                } else if cancellation.is_cancelled() {
                    DriveFailure::Turn(TurnOutcome::Cancelled)
                } else {
                    failed("model_output.admission", error.to_string())
                }
            })?;
        let mut evidence = capture.finish(&snapshot)?;
        self.evidence
            .lock()
            .map_err(|_| fatal("evidence cache lock poisoned"))?
            .deduplicate(claim.session_id(), &mut evidence);
        let intent = self
            .publish_durable(
                claim,
                fold,
                vec![SessionFactBody::ModelIntent {
                    evidence,
                    price_quote: claim
                        .header()
                        .settings()
                        .pricing()
                        .resolve(&snapshot)
                        .cloned(),
                    purpose: purpose.clone(),
                    turn_id: claim.turn_id().clone(),
                    effect_id: effect_id.clone(),
                    snapshot: snapshot.clone(),
                }],
            )
            .await?;

        for fact in intent.facts() {
            self.evidence
                .lock()
                .map_err(|_| fatal("evidence cache lock poisoned"))?
                .observe(claim.session_id(), fact);
        }
        self.publish_durable(
            claim,
            fold,
            vec![SessionFactBody::ModelStarted {
                turn_id: claim.turn_id().clone(),
                effect_id: effect_id.clone(),
            }],
        )
        .await?;

        live.started().map_err(fatal)?;

        let combined = combine_cancellation(cancellation, stop);
        let started_stream = tokio::select! { biased;
            () = stop.cancelled() => Err(model_cancelled()),
            () = cancellation.cancelled() => Err(model_cancelled()),
            result = prepared.start(combined.token()) => result,
        };
        let stream = match started_stream {
            Ok(stream) => stream,
            Err(error) => futures_util::stream::once(async move { Err(error) }).boxed(),
        };
        let result = self
            .consume_model_stream(
                fold,
                ModelStreamContext {
                    purpose: purpose.event_purpose(),
                    claim,
                    effect_id: &effect_id,
                    snapshot: &snapshot,
                    retry_attempt,
                    cancellation,
                    stop,
                    combined,
                    live,
                },
                stream,
            )
            .await?;
        if let rsi_agent_session_protocol::ModelPurpose::ContextCompaction(plan) = purpose
            && let ModelAttempt::Output(_, output) = &result
        {
            if stop.is_cancelled() {
                return Err(DriveFailure::Stopped);
            }
            if cancellation.is_cancelled() || output.finish_reason == FinishReason::Cancelled {
                return Err(DriveFailure::Turn(TurnOutcome::Cancelled));
            }
            rsi_agent_context::validate_summary_output(&plan, output)
                .map_err(|error| context_failure("context.compaction_failed", &error))?;
            if !fold.summary_installed(&effect_id) {
                return Err(failed(
                    "context.compaction_failed",
                    "summary did not install against its exact source view",
                ));
            }
        }
        Ok(result)
    }

    #[allow(clippy::too_many_lines)] // Stream admission, cancellation and durable settlement share one owner.
    pub(super) async fn consume_model_stream(
        &self,
        fold: &mut ModelContextState,
        attempt: ModelStreamContext<'_>,
        mut stream: rsi_ai_protocol::LanguageStream,
    ) -> std::result::Result<ModelAttempt, DriveFailure> {
        let ModelStreamContext {
            purpose,
            claim,
            effect_id,
            snapshot,
            retry_attempt,
            cancellation,
            stop,
            combined,
            mut live,
        } = attempt;
        let mut assembler = LanguageAssembler::new();
        let mut evidence = rsi_agent_session_protocol::ModelAttemptBuilder::new();
        let mut last_live = tokio::time::Instant::now();
        let mut live_bytes = 0_usize;
        let mut capacity = false;
        loop {
            let received = tokio::select! {
                biased;
                () = cancellation.cancelled() => Err(model_cancelled()),
                () = stop.cancelled() => Err(model_cancelled()),
                () = tokio::time::sleep_until(last_live + Duration::from_millis(50)), if live_bytes > 0 => {
                    live.publish(evidence.prefix()).map_err(fatal)?;
                    last_live = tokio::time::Instant::now();
                    live_bytes = 0;
                    continue;
                }
                next = stream.next() => match next {
                    Some(value) => value,
                    None => Err(model_stream_error("Language stream ended without a terminal event")),
                },
            };
            let mut event = received.unwrap_or_else(|error| LanguageEvent::Failed {
                error,
                replay: None,
            });
            let elapsed = live.elapsed_ms();
            if let Err(error) = assembler.push(&event) {
                event = LanguageEvent::Failed {
                    error: model_stream_error(&error.to_string()),
                    replay: None,
                };
            }
            if let Err(error) = evidence.push(elapsed, &event) {
                combined.cancel();
                capacity = true;
                let failure = rsi_ai_protocol::AiError::new(
                    ErrorKind::OutputValidation,
                    rsi_ai_protocol::ErrorPhase::Assemble,
                    DispatchStatus::Dispatched,
                    bounded(&error.to_string()),
                )
                .map_err(fatal)?
                .with_provider_code("model_output.capacity")
                .map_err(fatal)?;
                event = LanguageEvent::Failed {
                    error: failure,
                    replay: None,
                };
                evidence.push(elapsed, &event).map_err(fatal)?;
            }
            live_bytes = live_bytes.saturating_add(match &event {
                LanguageEvent::ContentDelta { delta, .. } => match delta {
                    rsi_ai_protocol::ContentDelta::Text(value)
                    | rsi_ai_protocol::ContentDelta::Reasoning(value)
                    | rsi_ai_protocol::ContentDelta::ToolArguments(value) => value.len(),
                },
                _ => 0,
            });
            let terminal = matches!(
                event,
                LanguageEvent::Finished { .. } | LanguageEvent::Failed { .. }
            );
            if terminal
                || !matches!(event, LanguageEvent::ContentDelta { .. })
                || live_bytes >= 32 * 1024
                || last_live.elapsed() >= Duration::from_millis(50)
            {
                live.publish(evidence.prefix()).map_err(fatal)?;
                last_live = tokio::time::Instant::now();
                live_bytes = 0;
            }
            if !terminal {
                continue;
            }
            combined.cancel();
            let output = assembler.finish();
            let retain_context = match &event {
                LanguageEvent::Failed { error, .. } => {
                    !should_retry(snapshot, error, retry_attempt)
                        || cancellation.is_cancelled()
                        || stop.is_cancelled()
                }
                _ => true,
            };
            let attempt = evidence.finish(retain_context).map_err(fatal)?;
            let facts = self
                .publish_durable(
                    claim,
                    fold,
                    vec![SessionFactBody::ModelAttemptSettled {
                        purpose,
                        turn_id: claim.turn_id().clone(),
                        effect_id: effect_id.clone(),
                        attempt: attempt.clone(),
                    }],
                )
                .await?;

            live.committed(&facts).map_err(fatal)?;
            if capacity {
                return Err(failed(
                    "model_output.capacity",
                    "Model attempt evidence exceeded its finite capacity; accepted prefix is durable",
                ));
            }
            if stop.is_cancelled() {
                return Err(DriveFailure::Stopped);
            }
            if cancellation.is_cancelled() {
                return Err(DriveFailure::Turn(TurnOutcome::Cancelled));
            }
            if let LanguageEvent::Failed { error, .. } = event {
                return self
                    .retry_or_fail(snapshot, &error, retry_attempt, cancellation, stop)
                    .await;
            }
            return match output {
                Ok(output) => Ok(ModelAttempt::Output(effect_id.clone(), Box::new(output))),
                Err(LanguageAssemblyError::Incomplete { .. }) => Ok(ModelAttempt::Incomplete),
                Err(LanguageAssemblyError::Provider { error, .. }) => {
                    self.retry_or_fail(snapshot, &error, retry_attempt, cancellation, stop)
                        .await
                }
                Err(LanguageAssemblyError::Protocol(error)) => {
                    Err(failed(error.code(), error.to_string()))
                }
            };
        }
    }

    pub(super) async fn retry_or_fail(
        &self,
        snapshot: &PreparedCallSnapshot,
        error: &rsi_ai_protocol::AiError,
        retry_attempt: u8,
        cancellation: &CancellationToken,
        stop: &CancellationToken,
    ) -> std::result::Result<ModelAttempt, DriveFailure> {
        if error.kind() == ErrorKind::ContextLimit
            && error.dispatch_status() != DispatchStatus::Unknown
        {
            return Ok(ModelAttempt::ContextLimit);
        }
        if !should_retry(snapshot, error, retry_attempt) {
            return Err(ai_failure(error));
        }
        self.wait_retry(snapshot, error, retry_attempt, cancellation, stop)
            .await?;
        Ok(ModelAttempt::Retry)
    }

    #[allow(clippy::too_many_arguments)] // Keep durable claim/fold, live Jobs authority, policy, and both cancellation owners explicit.
    pub(super) async fn run_tool(
        &self,
        source_model_effect_id: &EffectId,
        claim: &TurnClaim,
        composition: &AgentCompositionPin,
        job_scope: Option<&JobScopeAuthority>,
        fold: &mut ModelContextState,
        call: ModelToolCall,
        scheduling: ToolScheduling,
        turn_policy: ResolvedTurnPolicy,
        cancellation: &CancellationToken,
        stop: &CancellationToken,
    ) -> std::result::Result<Arc<SessionFact>, DriveFailure> {
        let pending = self
            .prepare_tool_call(
                &rsi_agent_session_protocol::ToolOrigin::Model {
                    effect_id: source_model_effect_id.clone(),
                },
                claim,
                composition,
                call,
                scheduling,
                turn_policy,
                cancellation,
                stop,
            )
            .await?;
        let pending = match pending {
            ToolPreparation::Ready(pending) => pending,
            ToolPreparation::Rejected(body) => {
                return Ok(self
                    .publish_durable(claim, fold, vec![*body])
                    .await?
                    .into_facts()
                    .into_iter()
                    .next()
                    .expect("one Tool rejection"));
            }
        };
        let prepared = self
            .publish_tool_start(claim, composition, fold, pending)
            .await?;
        let identity = prepared.identity.clone();
        let (result, nested_results) = match self
            .start_tool(
                &prepared.effect_id,
                prepared.prepared,
                &identity,
                composition,
                claim,
                job_scope,
                scheduling,
                turn_policy,
                cancellation,
                stop,
                matches!(
                    prepared.intent.body(),
                    SessionFactBody::ToolIntent {
                        program_role: rsi_tools_protocol::ToolProgramRole::Coordinator,
                        ..
                    }
                )
                .then_some(&mut *fold),
            )
            .await
        {
            Ok(result) => result,
            Err(failure) => {
                if matches!(
                    composition.tools().query(&identity),
                    Ok(RetainedToolResult::Absent)
                ) {
                    self.clear_tracked_tool(claim, &identity);
                }
                return Err(failure);
            }
        };
        let fact = self
            .publish_tool_result(claim, composition, fold, prepared.intent, result)
            .await?;
        if !nested_results.is_empty() {
            self.run_contributions(
                claim,
                composition,
                fold,
                rsi_agent_composition_protocol::ContributionStage::AfterTools,
                &nested_results,
                cancellation,
                stop,
            )
            .await?;
        }
        Ok(fact)
    }

    #[allow(clippy::too_many_arguments)] // The batch shares the exact turn policy and live turn-scoped authorities.
    pub(super) async fn run_parallel_tools(
        &self,
        source_model_effect_id: &EffectId,
        claim: &TurnClaim,
        composition: &AgentCompositionPin,
        job_scope: Option<&JobScopeAuthority>,
        fold: &mut ModelContextState,
        calls: Vec<ModelToolCall>,
        turn_policy: ResolvedTurnPolicy,
        cancellation: &CancellationToken,
        stop: &CancellationToken,
    ) -> std::result::Result<Vec<Arc<SessionFact>>, DriveFailure> {
        let mut pending = Vec::with_capacity(calls.len());
        let mut rejections = vec![None; calls.len()];
        for (index, call) in calls.into_iter().enumerate() {
            match self
                .prepare_tool_call(
                    &rsi_agent_session_protocol::ToolOrigin::Model {
                        effect_id: source_model_effect_id.clone(),
                    },
                    claim,
                    composition,
                    call,
                    ToolScheduling::ParallelSafe,
                    turn_policy,
                    cancellation,
                    stop,
                )
                .await?
            {
                ToolPreparation::Ready(effect) => pending.push(effect),
                ToolPreparation::Rejected(body) => rejections[index] = Some(body),
            }
        }

        if pending.is_empty() {
            return Ok(self
                .publish_durable(
                    claim,
                    fold,
                    rejections.into_iter().flatten().map(|body| *body).collect(),
                )
                .await?
                .into_facts());
        }

        let prepared = self
            .publish_parallel_tool_starts(claim, composition, fold, pending)
            .await?;

        let outcomes = join_all(prepared.into_iter().map(|effect| async move {
            let effect_id = effect.effect_id;
            let identity = effect.identity;
            let result = self
                .start_tool(
                    &effect_id,
                    effect.prepared,
                    &identity,
                    composition,
                    claim,
                    job_scope,
                    ToolScheduling::ParallelSafe,
                    turn_policy,
                    cancellation,
                    stop,
                    None,
                )
                .await;
            (effect.intent, identity, result.map(|(result, _)| result))
        }))
        .await;

        let mut first_failure = None;
        let mut outcomes = outcomes.into_iter();
        let mut settled = Vec::with_capacity(rejections.len());
        for rejection in rejections {
            if let Some(body) = rejection {
                settled.extend(
                    self.publish_durable(claim, fold, vec![*body])
                        .await?
                        .into_facts(),
                );
                continue;
            }
            let (intent, identity, result) =
                outcomes.next().expect("one outcome per admitted call");
            match result {
                Ok(result) => {
                    match self
                        .publish_tool_result(claim, composition, fold, intent, result)
                        .await
                    {
                        Ok(fact) => settled.push(fact),
                        Err(DriveFailure::Stopped) => return Err(DriveFailure::Stopped),
                        Err(failure) => {
                            first_failure.get_or_insert(failure);
                        }
                    }
                }
                Err(failure) => {
                    if matches!(
                        composition.tools().query(&identity),
                        Ok(RetainedToolResult::Absent)
                    ) {
                        self.clear_tracked_tool(claim, &identity);
                    }
                    first_failure.get_or_insert(failure);
                }
            }
        }
        if let Some(failure) = first_failure {
            return Err(failure);
        }
        Ok(settled)
    }

    #[allow(clippy::too_many_arguments)] // Preparation binds one model call to its exact policy and cancellation authorities.
    #[allow(clippy::too_many_lines)] // Admission preserves producing request proof and exact Tool policy through preparation.
    pub(super) async fn prepare_tool_call(
        &self,
        origin: &rsi_agent_session_protocol::ToolOrigin,
        claim: &TurnClaim,
        composition: &AgentCompositionPin,
        call: ModelToolCall,
        scheduling: ToolScheduling,
        turn_policy: ResolvedTurnPolicy,
        cancellation: &CancellationToken,
        stop: &CancellationToken,
    ) -> std::result::Result<ToolPreparation, DriveFailure> {
        let (effect_id, arguments) = prepare_tool_effect(&call).map_err(|failure| *failure)?;
        let name = call.name;
        let program_role = composition
            .tools()
            .program_role(&name)
            .unwrap_or(rsi_tools_protocol::ToolProgramRole::Unavailable);
        let mut prepared = composition
            .tools()
            .prepare(
                effect_id.as_str(),
                ToolCall {
                    id: call.id,
                    name: name.clone(),
                    arguments: arguments.clone(),
                },
            )
            .map_err(|error| tool_failure(&error))?;
        let identity = prepared.identity().clone();
        // The application worker runs as the operator, independently of the
        // sandbox chosen for native processes in this Turn.
        let (execution_mode, middleware) = match composition.request_tool_route(&name) {
            rsi_agent_composition_protocol::RequestToolRoute::Application => {
                (SandboxMode::DangerFullAccess, None)
            }
            rsi_agent_composition_protocol::RequestToolRoute::Native(middleware) => {
                (turn_policy.sandbox, middleware)
            }
        };
        let (require_approval, rejection) = self
            .tool_policy_decision(
                claim,
                composition,
                rsi_agent_composition_protocol::ToolPolicyRequest {
                    identity: &identity,
                    name: &name,
                    arguments: &arguments,
                    sandbox: execution_mode,
                    require_approval: turn_policy.require_approval,
                },
                origin,
                cancellation,
                stop,
            )
            .await?;
        if let Some(rejection) = rejection {
            return Ok(ToolPreparation::Rejected(Box::new(
                SessionFactBody::ToolRejected {
                    additional_contexts: Vec::new(),
                    origin: origin.clone(),
                    program_role,
                    turn_id: claim.turn_id().clone(),
                    effect_id,
                    identity,
                    name,
                    arguments,
                    rejection,
                },
            )));
        }
        let combined = combine_cancellation(cancellation, stop);
        let mut extensions = rsi_tools_protocol::ToolExecutionExtensions::default();
        if let Some(lease) = claim.execution() {
            extensions = extensions.with(Arc::new(lease.clone())).map_err(fatal)?;
        }
        let cwd = std::path::PathBuf::from(claim.header().canonical_cwd());
        let execution_review = prepared
            .prepare_execution(ToolStart {
                cancellation: combined.token(),
                policy: ToolExecutionPolicy {
                    mode: turn_policy.sandbox,
                    cwd: cwd.clone(),
                    workspace: cwd,
                },
                sandbox: self.sandbox.clone(),
                job_scope: None,
                extensions,
            })
            .await
            .map_err(|error| tool_failure(&error))?;
        let token = combined.token();
        let plan = prepared.native_plan();
        let mut gate = if let (Some(source), Some(plan)) = (middleware, plan.as_ref()) {
            Some(policy_stage(Arc::clone(source).prepare(plan.call, plan.output, &token)).await?)
        } else {
            None
        };
        let reason = match &gate {
            Some(ToolGate::Ask { reason, .. }) => Some(bounded(reason)),
            Some(ToolGate::Decided(ToolDecision::Denied { .. })) => None,
            _ if require_approval => Some("Required by the execution policy".into()),
            _ => None,
        };
        let mut approval = None;
        if let Some(reason) = reason {
            let request = ApprovalRequest {
                subject: ApprovalSubject::new(
                    claim.session_id().as_str(),
                    claim.turn_id().as_str(),
                    effect_id.as_str(),
                )
                .map_err(|error| failed("approval.invalid_subject", error.to_string()))?,
                id: format!("{}:{}", claim.turn_id().as_str(), effect_id.as_str()),
                action: format!("run tool {name}"),
                reason,
                review: Some(
                    tool_approval_review(
                        claim,
                        &arguments,
                        &identity,
                        execution_mode,
                        execution_review,
                    )
                    .map_err(fatal)?,
                ),
            };
            approval = self
                .request_tool_approval(claim, request, gate.as_mut(), &token, stop)
                .await?;
            if approval.is_none() {
                gate = Some(ToolGate::Decided(ToolDecision::Denied {
                    reason: "Application withdrew its native Tool invocation during approval"
                        .into(),
                    additional_contexts: Vec::new(),
                }));
            }
        }
        let denied = approval
            .as_ref()
            .is_some_and(|outcome| outcome.decision == ApprovalDecision::Deny);
        let decision = match gate {
            Some(ToolGate::Ask { pending, .. }) => {
                let decision = policy_stage(pending.answer(!denied, &token)).await;
                // A user's refusal remains conclusive if the application loses its answer.
                if denied {
                    decision.ok()
                } else {
                    Some(decision?)
                }
            }
            Some(ToolGate::Decided(decision)) => Some(decision),
            None => None,
        };
        let (rejection, additional_contexts) = match (approval, decision) {
            (Some(outcome), decision) if outcome.decision == ApprovalDecision::Deny => {
                let messages = match decision {
                    Some(ToolDecision::Denied {
                        additional_contexts,
                        ..
                    }) => additional_contexts,
                    _ => Vec::new(),
                };
                (
                    rsi_agent_session_protocol::ToolRejection::ApprovalDenied { outcome },
                    messages,
                )
            }
            (
                _,
                Some(ToolDecision::Denied {
                    reason,
                    additional_contexts,
                }),
            ) => (
                rsi_agent_session_protocol::ToolRejection::PolicyDenied {
                    contribution_id: rsi_agent_session_protocol::ContributionId::new(
                        "rsi.application.tool-policy",
                    )
                    .map_err(fatal)?,
                    reason: bounded(&reason),
                },
                additional_contexts,
            ),
            (approval, decision) => {
                if let (Some(plan), Some(ToolDecision::Ready(continuation))) = (plan, decision) {
                    *plan.continuation = Some(continuation);
                }
                return Ok(ToolPreparation::Ready(PendingToolEffect {
                    origin: origin.clone(),
                    program_role,
                    effect_id,
                    identity,
                    name,
                    arguments,
                    approval,
                    parallel_safe: scheduling == ToolScheduling::ParallelSafe,
                    prepared,
                }));
            }
        };
        Ok(ToolPreparation::Rejected(Box::new(
            SessionFactBody::ToolRejected {
                additional_contexts: if matches!(
                    origin,
                    rsi_agent_session_protocol::ToolOrigin::Model { .. }
                ) {
                    additional_contexts
                } else {
                    Vec::new()
                },
                origin: origin.clone(),
                program_role,
                turn_id: claim.turn_id().clone(),
                effect_id,
                identity,
                name,
                arguments,
                rejection,
            },
        )))
    }

    pub(super) async fn publish_tool_start(
        &self,
        claim: &TurnClaim,
        composition: &AgentCompositionPin,
        fold: &mut ModelContextState,
        pending: PendingToolEffect,
    ) -> std::result::Result<PreparedToolEffect, DriveFailure> {
        let PendingToolEffect {
            origin,
            program_role,
            effect_id,
            identity,
            name,
            arguments,
            approval,
            parallel_safe,
            prepared,
        } = pending;
        let intent = self
            .publish_durable(
                claim,
                fold,
                vec![SessionFactBody::ToolIntent {
                    origin,
                    program_role,
                    turn_id: claim.turn_id().clone(),
                    effect_id: effect_id.clone(),
                    identity: identity.clone(),
                    name,
                    arguments,
                    approval,
                    parallel_safe,
                }],
            )
            .await?;

        self.publish_durable(
            claim,
            fold,
            vec![SessionFactBody::ToolStarted {
                turn_id: claim.turn_id().clone(),
                effect_id: effect_id.clone(),
                identity: identity.clone(),
            }],
        )
        .await?;

        self.track_tool(claim, composition.clone(), identity.clone());
        Ok(PreparedToolEffect {
            intent: intent
                .into_facts()
                .into_iter()
                .next()
                .expect("one Tool intent"),
            effect_id,
            identity,
            prepared,
        })
    }

    pub(super) async fn publish_parallel_tool_starts(
        &self,
        claim: &TurnClaim,
        composition: &AgentCompositionPin,
        fold: &mut ModelContextState,
        pending: Vec<PendingToolEffect>,
    ) -> std::result::Result<Vec<PreparedToolEffect>, DriveFailure> {
        let mut intents = Vec::with_capacity(pending.len());
        let mut prepared = Vec::with_capacity(pending.len());
        for effect in pending {
            let PendingToolEffect {
                origin,
                program_role,
                effect_id,
                identity,
                name,
                arguments,
                approval,
                parallel_safe,
                prepared: prepared_call,
            } = effect;
            intents.push(SessionFactBody::ToolIntent {
                origin,
                program_role,
                turn_id: claim.turn_id().clone(),
                effect_id: effect_id.clone(),
                identity: identity.clone(),
                name,
                arguments,
                approval,
                parallel_safe,
            });
            prepared.push((effect_id, identity, prepared_call));
        }

        let intents = self.publish_durable(claim, fold, intents).await?;

        let prepared: Vec<_> = prepared
            .into_iter()
            .zip(intents.into_facts())
            .map(
                |((effect_id, identity, prepared), intent)| PreparedToolEffect {
                    intent,
                    effect_id,
                    identity,
                    prepared,
                },
            )
            .collect();
        let starts = prepared
            .iter()
            .map(|effect| SessionFactBody::ToolStarted {
                turn_id: claim.turn_id().clone(),
                effect_id: effect.effect_id.clone(),
                identity: effect.identity.clone(),
            })
            .collect();
        self.publish_durable(claim, fold, starts).await?;

        for effect in &prepared {
            self.track_tool(claim, composition.clone(), effect.identity.clone());
        }
        Ok(prepared)
    }

    #[allow(clippy::too_many_lines)] // Publication, exact receipt verification and notification must share one result owner.
    pub(super) async fn publish_tool_result(
        &self,
        claim: &TurnClaim,
        composition: &AgentCompositionPin,
        fold: &mut ModelContextState,
        intent: Arc<SessionFact>,
        result: ToolResult,
    ) -> std::result::Result<Arc<SessionFact>, DriveFailure> {
        let SessionFactBody::ToolIntent {
            effect_id,
            identity,
            ..
        } = intent.body()
        else {
            return Err(fatal("settlement lacks a ToolIntent"));
        };
        let mut settlement = self
            .tool_settlement_proposals(claim, composition, &intent, &result)
            .await?;
        let body = SessionFactBody::ToolResult {
            turn_id: claim.turn_id().clone(),
            effect_id: effect_id.clone(),
            identity: identity.clone(),
            result,

            conclusion: settlement.conclusion,
        };
        let mut committed_domains = None;
        let returned = if settlement.domains.is_empty() {
            let facts = self.publish_durable(claim, fold, vec![body]).await?;

            facts
                .into_facts()
                .into_iter()
                .next()
                .expect("one Tool result")
        } else {
            let request_id = rsi_agent_session_protocol::DomainRequestId::new(effect_id.as_str())
                .map_err(fatal)?;
            let receipt = self
                .turns
                .commit_domains(
                    claim,
                    rsi_agent_turn_protocol::DomainMutation {
                        guards: vec![],
                        require_uncancelled_turn: settlement.require_uncancelled_turn,
                        request_id,
                        proposals: settlement.domains,
                        facts: vec![body],
                    },
                )
                .await;
            let receipt = match receipt {
                Ok(receipt) => receipt,
                Err(error) => {
                    // These errors are issued before a domain candidate can be installed.
                    // Other failures require canonical reconciliation, even when the same
                    // error name can also originate before publication.
                    if matches!(
                        error,
                        TurnError::DomainRevisionConflict { .. } | TurnError::Capacity
                    ) {
                        super::tool_settlement::notify_publications(
                            &mut settlement.publications,
                            None,
                        )
                        .await?;
                    }
                    return Err(crate::execution_support::turn_failure(error));
                }
            };
            let span = receipt
                .commit()
                .fact_span()
                .filter(|span| span.count() == 1)
                .ok_or_else(|| fatal("Tool settlement receipt lacks its exact result span"))?;
            let page = self
                .turns
                .read_facts(claim, span.first_seq() - 1, 1)
                .await
                .map_err(fatal)?;
            let fact = page.facts.into_iter().next().filter(|fact| fact.seq() == span.first_seq()
                && matches!(fact.body(), SessionFactBody::ToolResult { effect_id: actual, identity: exact, .. } if actual == effect_id && exact == identity))
                .ok_or_else(|| fatal("Tool settlement receipt does not resolve to its exact result"))?;
            self.sync_fold(claim, fold).await?;
            committed_domains = Some(receipt);
            fact
        };
        let evidence = rsi_agent_composition_protocol::PublicationReceipt {
            session_id: claim.session_id(),
            facts: std::slice::from_ref(&returned),
            domain_commit: committed_domains
                .as_ref()
                .map(|receipt| (receipt.control_seq(), receipt.commit())),
        };
        super::tool_settlement::notify_publications(&mut settlement.publications, Some(&evidence))
            .await?;
        composition
            .tools()
            .commit(identity)
            .map_err(|error| tool_failure(&error))?;
        self.clear_tracked_tool(claim, identity);
        if matches!(
            returned.body(),
            SessionFactBody::ToolResult {
                conclusion: Some(_),
                ..
            }
        ) {
            return Err(DriveFailure::Turn(TurnOutcome::Completed));
        }
        Ok(returned)
    }

    pub(super) fn track_tool(
        &self,
        claim: &TurnClaim,
        composition: AgentCompositionPin,
        identity: ToolResultIdentity,
    ) {
        let key = (claim.session_id().clone(), claim.turn_id().clone());
        let tracker = self
            .controlled_work
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&key)
            .cloned();
        let mut settlement = tracker.as_ref().map(controlled_work::Tracker::guard);
        self.active_tools
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .entry(key)
            .or_default()
            .entry(identity)
            .or_insert_with(|| TrackedTool {
                composition,
                settlement: settlement.take(),
            });
        // An existing identity already owns its guard. Discarding this unused,
        // confirmed reservation must not mark that work as lost.
        if let Some(unused) = settlement {
            unused.confirm();
        }
    }

    pub(super) fn clear_tracked_tool(&self, claim: &TurnClaim, identity: &ToolResultIdentity) {
        let key = (claim.session_id().clone(), claim.turn_id().clone());
        let tracked = {
            let mut active = self
                .active_tools
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let tracked = active
                .get_mut(&key)
                .and_then(|tools| tools.remove(identity));
            if active
                .get(&key)
                .is_some_and(std::collections::BTreeMap::is_empty)
            {
                active.remove(&key);
            }
            tracked
        };
        if let Some(tracked) = tracked
            && let Some(guard) = tracked.settlement
        {
            guard.confirm();
        }
    }

    pub(super) fn retire_tracked_tools(&self, claim: &TurnClaim, stop: &CancellationToken) {
        let mut active = self
            .active_tools
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let tracked = active
            .remove(&(claim.session_id().clone(), claim.turn_id().clone()))
            .unwrap_or_default();
        drop(active);
        for (identity, tracked) in tracked {
            let composition = tracked.composition;
            let settlement_guard = tracked.settlement;
            let tools = composition.tools();
            let stop = stop.clone();
            let wait = self.config.retained_tool_wait();
            let task = tokio::spawn(async move {
                let _composition = composition;
                let settlement = tools.wait(&identity, stop.clone());
                tokio::pin!(settlement);
                let retained = tokio::select! {
                    biased;
                    () = stop.cancelled() => None,
                    () = tokio::time::sleep(wait) => None,
                    retained = &mut settlement => Some(retained),
                };
                if matches!(
                    retained,
                    Some(Ok(
                        RetainedToolResult::Returned(_) | RetainedToolResult::Failed(_)
                    ))
                ) {
                    let _ignored = tools.commit(&identity);
                    if let Some(guard) = settlement_guard {
                        guard.confirm();
                    }
                }
            });
            self.retirement_tasks
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push(task);
        }
    }

    pub(super) fn reap_retirement_tasks(&self) {
        self.retirement_tasks
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .retain(|task| !task.is_finished());
    }

    pub(super) async fn abort_retirement_tasks(&self) {
        let tasks = std::mem::take(
            &mut *self
                .retirement_tasks
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        );
        for task in &tasks {
            task.abort();
        }
        for task in tasks {
            let _ignored = task.await;
        }
        self.observations.close().await;
    }

    #[allow(clippy::too_many_arguments)] // Start binds one prepared effect to its durable identity and exact turn-scoped authorities.
    pub(super) async fn start_tool_raw(
        &self,
        effect_id: &EffectId,
        prepared: Box<dyn PreparedToolCall>,
        identity: &ToolResultIdentity,
        composition: &AgentCompositionPin,
        claim: &TurnClaim,
        job_scope: Option<&JobScopeAuthority>,
        scheduling: ToolScheduling,
        sandbox_mode: SandboxMode,
        cancellation: &CancellationToken,
        stop: &CancellationToken,
        program: Option<rsi_agent_turn_protocol::ProgramToolCalls>,
    ) -> std::result::Result<ToolResult, DriveFailure> {
        let combined = combine_cancellation(cancellation, stop);
        let cwd = std::path::PathBuf::from(claim.header().canonical_cwd());
        let mut extensions = crate::execution_support::tool_start_extensions(
            self.turns.as_ref(),
            claim,
            effect_id,
            self.config.optional_tool_evidence_bytes,
        )
        .map_err(fatal)?;
        if scheduling == ToolScheduling::ExclusiveFinal
            && let Ok(parking) = EXECUTOR_LANE_PARKING.try_with(Clone::clone)
        {
            extensions = extensions
                .with(Arc::new(parking))
                .map_err(|error| fatal(error.to_string()))?;
        }
        if let Some(program) = program {
            let caller = self.turns.tool_caller(claim, effect_id).map_err(fatal)?;
            let program = super::program::selected_dispatch(program, caller.source_tools());
            extensions = extensions.with(Arc::new(program)).map_err(fatal)?;
        }
        let result = tokio::select! {
            biased;
            result = prepared
            .start(ToolStart {
                cancellation: combined.token(),
                policy: ToolExecutionPolicy {
                    mode: sandbox_mode,
                    cwd: cwd.clone(),
                    workspace: cwd,
                },
                sandbox: Arc::clone(&self.sandbox),
                job_scope: job_scope.cloned(),
                extensions,
            })
            => result,
            () = cancellation.cancelled() => return Err(DriveFailure::Turn(TurnOutcome::Cancelled)),
            () = stop.cancelled() => return Err(DriveFailure::Stopped),
        };
        combined.cancel();
        if stop.is_cancelled() {
            return Err(DriveFailure::Stopped);
        }
        match result {
            Ok(result) => Ok(result),
            Err(error) => {
                let failure = tool_failure(&error);
                if matches!(
                    composition.tools().query(identity),
                    Ok(RetainedToolResult::Failed(_))
                ) {
                    let DriveFailure::Turn(outcome) = failure else {
                        return Err(failure);
                    };
                    return Err(DriveFailure::SettledTool {
                        outcome,
                        identity: identity.clone(),
                    });
                }
                Err(failure)
            }
        }
    }

    #[allow(clippy::too_many_arguments)] // Exact prepared review and both cancellation owners remain explicit.
    async fn request_tool_approval(
        &self,
        claim: &TurnClaim,
        request: ApprovalRequest,
        gate: Option<&mut ToolGate>,
        token: &CancellationToken,
        stop: &CancellationToken,
    ) -> std::result::Result<Option<rsi_approval_protocol::ApprovalOutcome>, DriveFailure> {
        let parking = EXECUTOR_LANE_PARKING
            .try_with(Clone::clone)
            .map_err(|error| fatal(error.to_string()))?;
        let waiting = self
            .turns
            .park_human_wait(claim, parking)
            .await
            .map_err(execution_support::turn_failure)?;
        let ask_stop = token.child_token();
        let ask = self.approval.ask(request, ask_stop.clone());
        tokio::pin!(ask);
        let outcome = tokio::select! {
            biased;
            () = async {
                match gate {
                    Some(ToolGate::Ask { pending, .. }) => pending.withdrawn().await,
                    Some(ToolGate::Decided(ToolDecision::Ready(call))) => call.withdrawn().await,
                    _ => std::future::pending().await,
                }
            } => { ask_stop.cancel(); let _ = ask.await; None },
            outcome = &mut ask => Some(outcome),
        };
        let resumed = waiting.resume(token.clone()).await;
        if stop.is_cancelled() {
            return Err(DriveFailure::Stopped);
        }
        resumed.map_err(execution_support::turn_failure)?;
        match outcome.transpose() {
            Ok(outcome) => Ok(outcome),
            Err(ApprovalError::Cancelled) if token.is_cancelled() => {
                Err(DriveFailure::Turn(TurnOutcome::Cancelled))
            }
            Err(error) => Err(failed("approval.failed", error.to_string())),
        }
    }

    pub(super) async fn recover_tool(
        &self,
        claim: &TurnClaim,
        composition: &AgentCompositionPin,
        fold: &mut ModelContextState,
        intent: Arc<SessionFact>,
        stop: &CancellationToken,
    ) -> std::result::Result<(), DriveFailure> {
        let SessionFactBody::ToolIntent { identity, .. } = intent.body() else {
            return Err(fatal("retained settlement lacks a ToolIntent"));
        };
        let tools = composition.tools();
        self.track_tool(claim, composition.clone(), identity.clone());
        let cancellation = self.turns.cancellation(claim).map_err(fatal)?;
        let retained = tokio::select! {
            biased;
            result = tools.wait(identity, stop.clone()) => result,
            () = cancellation.cancelled() => return Err(DriveFailure::Turn(TurnOutcome::Cancelled)),
            () = stop.cancelled() => return Err(DriveFailure::Stopped),
        };
        if stop.is_cancelled() {
            return Err(DriveFailure::Stopped);
        }
        match retained.map_err(|error| tool_failure(&error))? {
            RetainedToolResult::Returned(result) => {
                self.publish_tool_result(claim, composition, fold, intent, result)
                    .await?;
                Ok(())
            }
            RetainedToolResult::Failed(failure) => {
                let outcome = match failure.kind {
                    RetainedToolFailureKind::OutcomeUnknown => TurnOutcome::Interrupted {
                        effect: Some(EffectKind::Tool),
                        reason: bounded(&failure.summary),
                    },
                    RetainedToolFailureKind::Cancelled => {
                        if self
                            .turns
                            .cancellation(claim)
                            .map_err(fatal)?
                            .is_cancelled()
                        {
                            TurnOutcome::Cancelled
                        } else {
                            TurnOutcome::Interrupted {
                                    effect: Some(EffectKind::Tool),
                                    reason: "retained Tool call was cancelled by an executor generation change".into(),
                                }
                        }
                    }
                    RetainedToolFailureKind::Timeout | RetainedToolFailureKind::Execution => {
                        TurnOutcome::Failed {
                            code: "tool.execution".into(),
                            message: bounded(&failure.summary),
                        }
                    }
                };
                Err(DriveFailure::SettledTool {
                    outcome,
                    identity: identity.clone(),
                })
            }
            RetainedToolResult::Absent => Err(DriveFailure::Turn(TurnOutcome::Interrupted {
                effect: Some(EffectKind::Tool),
                reason: "exact retained Tool result is absent from its owner generation".into(),
            })),
            RetainedToolResult::Pending => Err(fatal(
                "Tool Runtime wait returned before the retained invocation settled",
            )),
        }
    }

    pub(super) async fn wait_retry(
        &self,
        snapshot: &PreparedCallSnapshot,
        error: &rsi_ai_protocol::AiError,
        attempt: u8,
        cancellation: &CancellationToken,
        stop: &CancellationToken,
    ) -> std::result::Result<(), DriveFailure> {
        let delay = retry_delay(snapshot, error, attempt);
        tokio::select! {
            () = tokio::time::sleep(delay) => Ok(()),
            () = cancellation.cancelled() => Err(DriveFailure::Turn(TurnOutcome::Cancelled)),
            () = stop.cancelled() => Err(DriveFailure::Stopped),
        }
    }

    pub(super) async fn finish(
        &self,
        claim: &TurnClaim,
        job_scope: Option<&JobScopeAuthority>,
        outcome: TurnOutcome,
    ) -> std::result::Result<Arc<SessionFact>, DriveFailure> {
        let outcome = self.resolve_outcome(claim, job_scope, outcome).await;
        publish_terminal(self.turns.as_ref(), &self.config, claim, outcome).await
    }

    pub(super) async fn resolve_outcome(
        &self,
        claim: &TurnClaim,
        job_scope: Option<&JobScopeAuthority>,
        outcome: TurnOutcome,
    ) -> TurnOutcome {
        let context = TurnFinalizationContext {
            session_id: claim.session_id().clone(),
            turn_id: claim.turn_id().clone(),
            job_scope: job_scope.cloned(),
        };
        let finalized = tokio::time::timeout(
            self.config.finalization_wait(),
            self.finalization.finalize(&context),
        )
        .await;
        let tracker = self
            .controlled_work
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&(claim.session_id().clone(), claim.turn_id().clone()))
            .cloned();
        if let Some(tracker) = tracker {
            tracker.finalized(matches!(&finalized, Ok(Ok(_))));
        }
        match finalized {
            Ok(Ok(report)) => match report.completion_blocker() {
                Some(blocker) => {
                    apply_finalization_failure(outcome, blocker.code(), blocker.message(), false)
                }
                None => outcome,
            },
            Ok(Err(TurnFinalizationError::OutcomeUnknown)) => TurnOutcome::Interrupted {
                effect: Some(EffectKind::Tool),
                reason: "Background effect outcome is unknown; do not replay".into(),
            },
            Ok(Err(TurnFinalizationError::Failed { code, message })) => {
                apply_finalization_failure(outcome, &bounded(&code), &bounded(&message), true)
            }
            Ok(Err(TurnFinalizationError::Invalid(message))) => {
                apply_finalization_failure(outcome, "turn.finalization", &bounded(&message), true)
            }
            Err(_) => apply_finalization_failure(
                outcome,
                "turn.finalization_timeout",
                &format!(
                    "turn finalization exceeded {} ms",
                    self.config.finalization_wait_ms
                ),
                true,
            ),
        }
    }

    pub(super) const fn context_limits(&self) -> ContextLimits {
        ContextLimits {
            max_messages: self.config.max_context_messages,
            max_bytes: self.config.max_context_bytes,
        }
    }

    pub(super) async fn finish_context_error(
        &self,
        claim: &TurnClaim,
        job_scope: Option<&JobScopeAuthority>,
        message: String,
    ) {
        let _ignored = self
            .finish(
                claim,
                job_scope,
                failure_outcome("context.invalid", message),
            )
            .await;
    }

    pub(super) fn request_checkpoint(&self, claim: &TurnClaim, composition: &AgentCompositionPin) {
        let _ignored = self.checkpoints.schedule(CheckpointRequest::new(
            claim.clone(),
            self.context_limits(),
            composition.clone(),
            self.context_budget.clone(),
        ));
    }

    pub(super) async fn publish_durable(
        &self,
        claim: &TurnClaim,
        fold: &mut ModelContextState,
        bodies: Vec<SessionFactBody>,
    ) -> std::result::Result<rsi_agent_turn_protocol::PublishedFacts, DriveFailure> {
        let receipt = publish_nonterminal_with_capacity_retry(
            self.turns.as_ref(),
            &self.config,
            claim,
            bodies,
        )
        .await?;
        let facts = receipt.facts();
        if facts
            .first()
            .is_some_and(|fact| fact.seq() != fold.position().through_seq + 1)
        {
            self.sync_fold(claim, fold).await?;
        }
        if let Some(unapplied) = facts
            .iter()
            .position(|fact| fact.seq() > fold.position().through_seq)
        {
            fold.ingest(ContextPage::Canonical(&facts[unapplied..]))
                .map_err(|error| context_failure("context.incremental", &error))?;
        }
        let seq = receipt
            .facts()
            .last()
            .ok_or_else(|| failed("executor.empty_publish", "publication returned no Facts"))?
            .seq();
        self.flush_durable(claim, seq).await?;
        Ok(receipt)
    }

    pub(super) async fn flush_durable(
        &self,
        claim: &TurnClaim,
        through_seq: u64,
    ) -> std::result::Result<u64, DriveFailure> {
        crate::execution_support::flush_execution_prefix(
            self.turns.as_ref(),
            &self.config,
            claim,
            through_seq,
        )
        .await
    }

    pub(super) async fn sync_fold(
        &self,
        claim: &TurnClaim,
        fold: &mut ModelContextState,
    ) -> std::result::Result<(), DriveFailure> {
        loop {
            let after_seq = fold.position().through_seq;
            let page = self
                .turns
                .read_facts(
                    claim,
                    after_seq,
                    rsi_agent_session_protocol::MAXIMUM_FACTS_PER_READ,
                )
                .await
                .map_err(fatal)?;
            if page.through_seq == after_seq {
                return Ok(());
            }
            fold.ingest(ContextPage::ClaimVisible {
                facts: &page.facts,
                through_seq: page.through_seq,
            })
            .map_err(|error| context_failure("context.incremental", &error))?;
        }
    }

    pub(super) async fn load_claim(
        &self,
        claim: &TurnClaim,
        fold: &mut ModelContextState,
    ) -> std::result::Result<ScannedTurn, DriveFailure> {
        let mut operation = self.checkpoints.diagnostics.operation(
            claim.session_id(),
            crate::checkpoint_diagnostics::Phase::Recovery,
        );
        operation.record.scan.sequence_lag = claim.live_seq();
        let result = self
            .load_claim_measured(claim, fold, &mut operation.record.scan)
            .await;
        operation.record.outcome = if result.is_ok() {
            crate::checkpoint_diagnostics::Outcome::Ready
        } else {
            crate::checkpoint_diagnostics::Outcome::Failed
        };
        result
    }

    async fn load_claim_measured(
        &self,
        claim: &TurnClaim,
        fold: &mut ModelContextState,
        scan: &mut crate::checkpoint_diagnostics::Scan,
    ) -> std::result::Result<ScannedTurn, DriveFailure> {
        let mut state = ScannedTurn::default();
        let mut cursor = 0;
        let mut restored_checkpoint = false;
        let checkpoint = match checkpoint::read_checkpoint(
            &self.turns,
            claim.session_id(),
            &self.context_budget,
        )
        .await
        {
            Err(rsi_agent_turn_protocol::TurnError::Capacity) => {
                return Err(context_failure(
                    "context.restore",
                    &rsi_agent_context::ContextError::Capacity,
                ));
            }
            result => result.ok().flatten(),
        };
        if let Some(checkpoint) = checkpoint
            && checkpoint.through_seq < claim.accepted_seq()
            && checkpoint.through_seq <= claim.live_seq()
            && let Some(restored) = checkpoint::restore_candidate(fold, &checkpoint.bytes)
                .map_err(|error| context_failure("context.restore", &error))?
            && restored.position().through_seq == checkpoint.through_seq
            && restored.position().fact_prefix_sha256() == checkpoint.fact_prefix_sha256
            && claim
                .header()
                .fingerprint()
                .is_ok_and(|fingerprint| fingerprint == checkpoint.header_fingerprint)
        {
            *fold = restored;
            cursor = checkpoint.through_seq;
            restored_checkpoint = true;
            scan.checkpoint_seq = cursor;
            scan.sequence_lag = claim.live_seq().saturating_sub(cursor);
        }
        if !restored_checkpoint {
            self.load_fork_seed(claim, fold, scan).await?;
        }
        loop {
            let page = self
                .turns
                .read_facts(
                    claim,
                    cursor,
                    rsi_agent_session_protocol::MAXIMUM_FACTS_PER_READ,
                )
                .await
                .map_err(fatal)?;
            scan.page(false, &page.facts);
            if page.through_seq == cursor {
                return Ok(state);
            }
            cursor = page.through_seq;
            fold.ingest(ContextPage::ClaimVisible {
                facts: &page.facts,
                through_seq: page.through_seq,
            })
            .map_err(|error| context_failure("context.invalid", &error))?;
            {
                let mut evidence = self
                    .evidence
                    .lock()
                    .map_err(|_| fatal("evidence cache lock poisoned"))?;
                for fact in &page.facts {
                    evidence.observe(claim.session_id(), fact);
                }
            }
            scan_turn(claim, &mut state, &page.facts)
                .map_err(|message| failed("executor.invalid_history", message))?;
        }
    }

    pub(super) async fn load_fork_seed(
        &self,
        claim: &TurnClaim,
        fold: &mut ModelContextState,
        scan: &mut crate::checkpoint_diagnostics::Scan,
    ) -> std::result::Result<(), DriveFailure> {
        if claim.header().history_origin().is_none() {
            return Ok(());
        }
        let sources = self.turns.read_fork_sources(claim).await.map_err(fatal)?;
        for source in &sources {
            fold.ingest(ContextPage::ForkSource(source))
                .map_err(|error| context_failure("context.invalid_fork", &error))?;
            let origin = source
                .history_origin()
                .ok_or_else(|| failed("context.invalid_fork", "source has no fork interval"))?;
            let mut cursor = origin.resolved_after_seq;
            loop {
                let page = self
                    .turns
                    .read_fork_facts(
                        claim,
                        source.session_id(),
                        cursor,
                        rsi_agent_session_protocol::MAXIMUM_FACTS_PER_READ,
                    )
                    .await
                    .map_err(fatal)?
                    .ok_or_else(|| {
                        failed(
                            "context.fork_unsupported",
                            "forked session has no inherited-history reader",
                        )
                    })?;
                scan.page(true, &page.facts);
                if page.through_parent_seq == cursor {
                    if cursor != page.terminal_parent_seq {
                        return Err(failed(
                            "context.fork_stalled",
                            "fork history reader made no progress",
                        ));
                    }
                    break;
                }
                fold.ingest(ContextPage::ForkSeed(&page.facts))
                    .map_err(|error| context_failure("context.invalid_fork", &error))?;
                cursor = page.through_parent_seq;
            }
        }
        fold.ingest(ContextPage::FinishSeed)
            .map_err(|error| context_failure("context.invalid_fork", &error))
    }
}

/// Only the two machine-driven policy exchanges share the contribution stage budget.
async fn policy_stage<T>(
    step: impl Future<Output = rsi_tools_protocol::Result<T>>,
) -> std::result::Result<T, DriveFailure> {
    tokio::time::timeout(contributions::STAGE_DEADLINE, step)
        .await
        .map_err(|_| {
            failed(
                "tool.policy_timeout",
                "Application Tool policy deadline elapsed before native start",
            )
        })?
        .map_err(|error| match error {
            ToolError::OutcomeUnknown => failed(
                "tool.policy_unavailable",
                "Application Tool policy disconnected before native start",
            ),
            _ => tool_failure(&error),
        })
}

fn tool_approval_review(
    claim: &TurnClaim,
    arguments: &serde_json::Value,
    identity: &ToolResultIdentity,
    execution_mode: SandboxMode,
    execution: Option<rsi_execution::ExecutionReview>,
) -> serde_json::Result<rsi_approval_protocol::ApprovalReview> {
    Ok(rsi_approval_protocol::ApprovalReview {
        execution,
        arguments: arguments.clone(),
        cwd: claim.header().canonical_cwd().to_owned(),
        sandbox: serde_json::to_value(execution_mode)?
            .as_str()
            .expect("sandbox enum is a string")
            .to_owned(),
        request_sha256: identity.request_sha256().to_owned(),
    })
}

fn model_cancelled() -> rsi_ai_protocol::AiError {
    rsi_ai_protocol::AiError::new(
        ErrorKind::Cancelled,
        rsi_ai_protocol::ErrorPhase::Stream,
        DispatchStatus::Dispatched,
        "Model attempt cancelled with its accepted prefix retained",
    )
    .expect("static model cancellation")
}
fn model_stream_error(message: &str) -> rsi_ai_protocol::AiError {
    rsi_ai_protocol::AiError::new(
        ErrorKind::Protocol,
        rsi_ai_protocol::ErrorPhase::Stream,
        DispatchStatus::Dispatched,
        bounded(message),
    )
    .expect("bounded model error")
}
