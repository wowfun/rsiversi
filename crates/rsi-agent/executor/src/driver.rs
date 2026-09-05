use super::*;

impl Driver {
    pub(super) async fn run_claim(&self, claim: TurnClaim, stop: &CancellationToken) {
        let job_scope = match self.acquire_job_scope(&claim) {
            Ok(scope) => Some(scope),
            Err(message) => {
                let _ignored = self
                    .finish(
                        &claim,
                        None,
                        failure_outcome("jobs.scope", bounded(&message)),
                    )
                    .await;
                let _ignored = self.turns.release(&claim);
                return;
            }
        };
        let composition = match self.turns.composition(&claim) {
            Ok(composition) => composition,
            Err(error) => {
                self.finish_context_error(&claim, job_scope.as_ref(), error.to_string())
                    .await;
                let _ignored = self.turns.release(&claim);
                return;
            }
        };
        let claim_stop = stop.child_token();
        let deadline_fired = Arc::new(AtomicBool::new(false));
        let elapsed = unix_now_ms().saturating_sub(claim.accepted_at_ms());
        let limit = claim.header().settings().turn_budget().maximum_elapsed_ms();
        let remaining = limit.saturating_sub(elapsed);
        let deadline_task = tokio::spawn({
            let deadline_fired = Arc::clone(&deadline_fired);
            let claim_stop = claim_stop.clone();
            async move {
                tokio::time::sleep(Duration::from_millis(remaining)).await;
                deadline_fired.store(true, Ordering::Release);
                claim_stop.cancel();
            }
        });
        let limits = self.context_limits();
        let mut fold = match ContextFold::with_limits(claim.header().clone(), limits) {
            Ok(fold) => fold,
            Err(error) => {
                deadline_task.abort();
                self.finish_context_error(&claim, job_scope.as_ref(), error.to_string())
                    .await;
                let _ignored = self.turns.release(&claim);
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
        deadline_task.abort();
        if elapsed_deadline_wins(deadline_fired.load(Ordering::Acquire), &drive) {
            let consumed = unix_now_ms()
                .saturating_sub(claim.accepted_at_ms())
                .max(limit);
            if self
                .finish_budget(
                    &claim,
                    job_scope.as_ref(),
                    BudgetDimension::Elapsed,
                    consumed,
                    limit,
                )
                .await
                .is_ok()
            {
                self.request_checkpoint(&claim);
                self.retire_tracked_tools(&claim, stop);
            }
            let _ignored = self.turns.release(&claim);
            return;
        }
        self.settle_drive(&claim, &composition, job_scope.as_ref(), drive, stop)
            .await;
        let _ignored = self.turns.release(&claim);
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
        match drive {
            Ok(()) | Err(DriveFailure::Stopped) => {}
            Err(DriveFailure::Turn(outcome)) => {
                if self.finish(claim, job_scope, outcome).await.is_ok() {
                    self.request_checkpoint(claim);
                    self.retire_tracked_tools(claim, stop);
                }
            }
            Err(DriveFailure::SettledTool { outcome, identity }) => {
                if self.finish(claim, job_scope, outcome).await.is_ok() {
                    self.request_checkpoint(claim);
                    let _ignored = composition.tools().commit(&identity);
                    self.clear_tracked_tool(claim, &identity);
                    self.retire_tracked_tools(claim, stop);
                }
            }
            Err(DriveFailure::Budget {
                dimension,
                consumed,
                limit,
            }) => {
                if self
                    .finish_budget(claim, job_scope, dimension, consumed, limit)
                    .await
                    .is_ok()
                {
                    self.request_checkpoint(claim);
                    self.retire_tracked_tools(claim, stop);
                }
            }
            Err(DriveFailure::DurableBudget {
                dimension,
                consumed,
                limit,
            }) => {
                if publish_terminal(
                    self.turns.as_ref(),
                    &self.config,
                    claim,
                    TurnOutcome::BudgetExceeded {
                        dimension,
                        consumed,
                        limit,
                    },
                )
                .await
                .is_ok()
                {
                    self.request_checkpoint(claim);
                    self.retire_tracked_tools(claim, stop);
                }
            }
            Err(DriveFailure::SettledToolBudget {
                dimension,
                consumed,
                limit,
                identity,
            }) => {
                if self
                    .finish_budget(claim, job_scope, dimension, consumed, limit)
                    .await
                    .is_ok()
                {
                    self.request_checkpoint(claim);
                    let _ignored = composition.tools().commit(&identity);
                    self.clear_tracked_tool(claim, &identity);
                    self.retire_tracked_tools(claim, stop);
                }
            }
            Err(DriveFailure::Fatal(message)) => {
                if self
                    .finish(
                        claim,
                        job_scope,
                        TurnOutcome::Failed {
                            code: "executor.internal".into(),
                            message,
                        },
                    )
                    .await
                    .is_ok()
                {
                    self.request_checkpoint(claim);
                    self.retire_tracked_tools(claim, stop);
                }
            }
        }
    }

    pub(super) async fn drive(
        &self,
        claim: &TurnClaim,
        composition: &AgentCompositionPin,
        job_scope: Option<&JobScopeAuthority>,
        stop: &CancellationToken,
        fold: &mut ContextFold,
    ) -> std::result::Result<(), DriveFailure> {
        let state = self.load_claim(claim, fold).await?;
        if state.terminal {
            return Ok(());
        }
        if let Some((dimension, consumed, limit)) = state.budget_exhausted {
            return Err(DriveFailure::DurableBudget {
                dimension,
                consumed,
                limit,
            });
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
            .unwrap_or_else(|| claim.header().settings().default_model().clone());
        self.run_language(
            claim,
            composition,
            job_scope,
            fold,
            model,
            turn_policy,
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
        fold: &mut ContextFold,
        model: ModelRef,
        turn_policy: ResolvedTurnPolicy,
        stop: &CancellationToken,
    ) -> std::result::Result<(), DriveFailure> {
        let mut retry_attempt = 0_u8;
        loop {
            if stop.is_cancelled() {
                return Err(DriveFailure::Stopped);
            }
            let cancellation = self.turns.cancellation(claim).map_err(fatal)?;
            if cancellation.is_cancelled() {
                return Err(DriveFailure::Turn(TurnOutcome::Cancelled));
            }
            if self
                .turns
                .enter_pending_step_messages(claim)
                .await
                .map_err(fatal)?
                > 0
            {
                self.sync_fold(claim, fold).await?;
                retry_attempt = 0;
            }
            if self
                .turns
                .refresh_workspace_context(claim)
                .await
                .map_err(fatal)?
                > 0
            {
                self.sync_fold(claim, fold).await?;
                retry_attempt = 0;
            }
            let output = match self
                .run_model_attempt(
                    claim,
                    composition,
                    fold,
                    &model,
                    retry_attempt,
                    &cancellation,
                    stop,
                )
                .await?
            {
                ModelAttempt::Retry => {
                    retry_attempt = retry_attempt.saturating_add(1);
                    continue;
                }
                ModelAttempt::Output(output) => *output,
            };
            if cancellation.is_cancelled()
                || matches!(output.finish_reason, FinishReason::Cancelled)
            {
                return Err(DriveFailure::Turn(TurnOutcome::Cancelled));
            }
            if self
                .turns
                .enter_pending_step_messages(claim)
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
            let definitions = tools.definitions();
            let mut scheduled = VecDeque::with_capacity(calls.len());
            let total_calls = calls.len();
            for (index, call) in calls.into_iter().enumerate() {
                let scheduling = definitions
                    .iter()
                    .find(|definition| definition.name() == call.name)
                    .map(rsi_tools_protocol::ToolDefinition::scheduling)
                    .ok_or_else(|| {
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
            while let Some((call, scheduling)) = scheduled.pop_front() {
                match scheduling {
                    ToolScheduling::Exclusive | ToolScheduling::ExclusiveFinal => {
                        self.run_tool(
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
                        .await?;
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
                        self.run_parallel_tools(
                            claim,
                            composition,
                            job_scope,
                            fold,
                            batch,
                            turn_policy,
                            &cancellation,
                            stop,
                        )
                        .await?;
                    }
                }
            }
        }
    }

    pub(super) async fn resume_effects(
        &self,
        claim: &TurnClaim,
        composition: &AgentCompositionPin,
        fold: &mut ContextFold,
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
                    effect_id,
                    identity,
                    started: true,
                } => {
                    self.recover_tool(claim, composition, fold, effect_id, identity, stop)
                        .await?;
                }
            }
        }
        Ok(())
    }

    pub(super) async fn run_image(
        &self,
        claim: &TurnClaim,
        fold: &mut ContextFold,
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
        let expected_outputs = usize::from(request.count());
        let prepared = self
            .image
            .prepare(model, request)
            .await
            .map_err(|error| image_ai_failure(&error, Vec::new()))?;
        let snapshot = prepared.snapshot().clone();
        let effect_id = next_effect_id().map_err(fatal)?;
        let intent = self
            .publish_apply(
                claim,
                fold,
                vec![SessionFactBody::ImageIntent {
                    turn_id: claim.turn_id().clone(),
                    effect_id: effect_id.clone(),
                    snapshot,
                }],
            )
            .await?;
        self.flush_last(claim, &intent).await?;
        let started = self
            .publish_apply(
                claim,
                fold,
                vec![SessionFactBody::ImageStarted {
                    turn_id: claim.turn_id().clone(),
                    effect_id: effect_id.clone(),
                }],
            )
            .await?;
        self.flush_last(claim, &started).await?;

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
        fold: &mut ContextFold,
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
                let reference = self
                    .media
                    .import_image(Arc::from(output.bytes))
                    .await
                    .map_err(|error| {
                        image_operation_failure(media.clone(), "media.commit", error.to_string())
                    })?;
                let published = self
                    .publish_apply(
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
                self.flush_last(claim, &published).await?;
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

    #[allow(clippy::too_many_arguments)] // One attempt binds the resident generation to its durable fold, model, retry ordinal, and cancellation fences.
    pub(super) async fn run_model_attempt(
        &self,
        claim: &TurnClaim,
        composition: &AgentCompositionPin,
        fold: &mut ContextFold,
        model: &ModelRef,
        retry_attempt: u8,
        cancellation: &CancellationToken,
        stop: &CancellationToken,
    ) -> std::result::Result<ModelAttempt, DriveFailure> {
        self.sync_fold(claim, fold).await?;
        let request = fold
            .request(self.config.limits(), composition.tools().definitions())
            .map_err(|error| failed("context.projection", error.to_string()))?;
        let prepared = self
            .language
            .prepare(model.clone(), request)
            .await
            .map_err(|error| ai_failure(&error))?;
        let snapshot = prepared.snapshot().clone();
        let effect_id = next_effect_id().map_err(fatal)?;
        let intent = self
            .publish_apply(
                claim,
                fold,
                vec![SessionFactBody::ModelIntent {
                    turn_id: claim.turn_id().clone(),
                    effect_id: effect_id.clone(),
                    snapshot: snapshot.clone(),
                }],
            )
            .await?;
        self.flush_last(claim, &intent).await?;
        let started = self
            .publish_apply(
                claim,
                fold,
                vec![SessionFactBody::ModelStarted {
                    turn_id: claim.turn_id().clone(),
                    effect_id: effect_id.clone(),
                }],
            )
            .await?;
        self.flush_last(claim, &started).await?;

        let combined = combine_cancellation(cancellation, stop);
        let stream = match prepared.start(combined.token()).await {
            Ok(stream) => stream,
            Err(error) => {
                combined.cancel();
                if stop.is_cancelled() {
                    return Err(DriveFailure::Stopped);
                }
                self.record_model_failure(claim, fold, &effect_id, error.clone())
                    .await?;
                return self
                    .retry_or_fail(&snapshot, &error, retry_attempt, cancellation, stop)
                    .await;
            }
        };
        self.consume_model_stream(
            fold,
            ModelStreamContext {
                claim,
                effect_id: &effect_id,
                snapshot: &snapshot,
                retry_attempt,
                cancellation,
                stop,
                combined,
            },
            stream,
        )
        .await
    }

    pub(super) async fn consume_model_stream(
        &self,
        fold: &mut ContextFold,
        attempt: ModelStreamContext<'_>,
        mut stream: rsi_ai_protocol::LanguageStream,
    ) -> std::result::Result<ModelAttempt, DriveFailure> {
        let ModelStreamContext {
            claim,
            effect_id,
            snapshot,
            retry_attempt,
            cancellation,
            stop,
            combined,
        } = attempt;
        let mut assembler = LanguageAssembler::new();
        let mut last_flush = tokio::time::Instant::now();
        loop {
            let event = match stream.next().await {
                Some(Ok(event)) => event,
                Some(Err(error)) => {
                    combined.cancel();
                    if stop.is_cancelled() {
                        return Err(DriveFailure::Stopped);
                    }
                    self.record_model_failure(claim, fold, effect_id, error.clone())
                        .await?;
                    return self
                        .retry_or_fail(snapshot, &error, retry_attempt, cancellation, stop)
                        .await;
                }
                None => {
                    combined.cancel();
                    return Err(failed(
                        "provider.missing_terminal",
                        "Language stream ended without a terminal event",
                    ));
                }
            };
            assembler
                .push(&event)
                .map_err(|error| failed(error.code(), error.to_string()))?;
            let terminal = matches!(
                event,
                LanguageEvent::Finished { .. } | LanguageEvent::Failed { .. }
            );
            let facts = self
                .publish_apply(
                    claim,
                    fold,
                    vec![SessionFactBody::ModelEvent {
                        turn_id: claim.turn_id().clone(),
                        effect_id: effect_id.clone(),
                        event,
                    }],
                )
                .await?;
            if terminal || last_flush.elapsed() >= STREAM_FLUSH_INTERVAL {
                self.flush_last(claim, &facts).await?;
                last_flush = tokio::time::Instant::now();
            }
            if terminal {
                combined.cancel();
                return match assembler.finish() {
                    Ok(output) => Ok(ModelAttempt::Output(Box::new(output))),
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
    }

    pub(super) async fn retry_or_fail(
        &self,
        snapshot: &PreparedCallSnapshot,
        error: &rsi_ai_protocol::AiError,
        retry_attempt: u8,
        cancellation: &CancellationToken,
        stop: &CancellationToken,
    ) -> std::result::Result<ModelAttempt, DriveFailure> {
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
        claim: &TurnClaim,
        composition: &AgentCompositionPin,
        job_scope: Option<&JobScopeAuthority>,
        fold: &mut ContextFold,
        call: ModelToolCall,
        scheduling: ToolScheduling,
        turn_policy: ResolvedTurnPolicy,
        cancellation: &CancellationToken,
        stop: &CancellationToken,
    ) -> std::result::Result<(), DriveFailure> {
        let pending = self
            .prepare_tool_call(
                claim,
                composition,
                call,
                scheduling,
                turn_policy.require_approval,
                cancellation,
                stop,
            )
            .await?;
        let prepared = self
            .publish_tool_start(claim, composition, fold, pending)
            .await?;
        let identity = prepared.identity.clone();
        let result = match self
            .start_tool(
                prepared.prepared,
                &identity,
                composition,
                claim,
                job_scope,
                scheduling,
                turn_policy.sandbox,
                cancellation,
                stop,
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
        self.publish_tool_result(
            claim,
            composition,
            fold,
            prepared.effect_id,
            identity,
            result,
        )
        .await
    }

    #[allow(clippy::too_many_arguments)] // The batch shares the exact turn policy and live turn-scoped authorities.
    pub(super) async fn run_parallel_tools(
        &self,
        claim: &TurnClaim,
        composition: &AgentCompositionPin,
        job_scope: Option<&JobScopeAuthority>,
        fold: &mut ContextFold,
        calls: Vec<ModelToolCall>,
        turn_policy: ResolvedTurnPolicy,
        cancellation: &CancellationToken,
        stop: &CancellationToken,
    ) -> std::result::Result<(), DriveFailure> {
        let mut pending = Vec::with_capacity(calls.len());
        for call in calls {
            pending.push(
                self.prepare_tool_call(
                    claim,
                    composition,
                    call,
                    ToolScheduling::ParallelSafe,
                    turn_policy.require_approval,
                    cancellation,
                    stop,
                )
                .await?,
            );
        }

        let prepared = self
            .publish_parallel_tool_starts(claim, composition, fold, pending)
            .await?;

        let outcomes = join_all(prepared.into_iter().map(|effect| async move {
            let effect_id = effect.effect_id;
            let identity = effect.identity;
            let result = self
                .start_tool(
                    effect.prepared,
                    &identity,
                    composition,
                    claim,
                    job_scope,
                    ToolScheduling::ParallelSafe,
                    turn_policy.sandbox,
                    cancellation,
                    stop,
                )
                .await;
            (effect_id, identity, result)
        }))
        .await;

        let mut first_failure = None;
        for (effect_id, identity, result) in outcomes {
            match result {
                Ok(result) => {
                    if let Err(failure) = self
                        .publish_tool_result(claim, composition, fold, effect_id, identity, result)
                        .await
                    {
                        if matches!(failure, DriveFailure::Stopped) {
                            return Err(failure);
                        }
                        first_failure.get_or_insert(failure);
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
        Ok(())
    }

    #[allow(clippy::too_many_arguments)] // Preparation binds one model call to its exact policy and cancellation authorities.
    pub(super) async fn prepare_tool_call(
        &self,
        claim: &TurnClaim,
        composition: &AgentCompositionPin,
        call: ModelToolCall,
        scheduling: ToolScheduling,
        require_approval: bool,
        cancellation: &CancellationToken,
        stop: &CancellationToken,
    ) -> std::result::Result<PendingToolEffect, DriveFailure> {
        let (effect_id, arguments) = prepare_tool_effect(&call).map_err(|failure| *failure)?;
        let approval = self
            .request_tool_approval(
                claim,
                &effect_id,
                &call.name,
                require_approval,
                cancellation,
                stop,
            )
            .await?;
        let name = call.name;
        let prepared = composition
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
        Ok(PendingToolEffect {
            effect_id,
            identity,
            name,
            arguments,
            approval,
            parallel_safe: scheduling == ToolScheduling::ParallelSafe,
            prepared,
        })
    }

    pub(super) async fn publish_tool_start(
        &self,
        claim: &TurnClaim,
        composition: &AgentCompositionPin,
        fold: &mut ContextFold,
        pending: PendingToolEffect,
    ) -> std::result::Result<PreparedToolEffect, DriveFailure> {
        let PendingToolEffect {
            effect_id,
            identity,
            name,
            arguments,
            approval,
            parallel_safe,
            prepared,
        } = pending;
        let intent = self
            .publish_apply(
                claim,
                fold,
                vec![SessionFactBody::ToolIntent {
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
        self.flush_last(claim, &intent).await?;
        let started = self
            .publish_apply(
                claim,
                fold,
                vec![SessionFactBody::ToolStarted {
                    turn_id: claim.turn_id().clone(),
                    effect_id: effect_id.clone(),
                    identity: identity.clone(),
                }],
            )
            .await?;
        self.flush_last(claim, &started).await?;
        self.track_tool(claim, composition.clone(), identity.clone());
        Ok(PreparedToolEffect {
            effect_id,
            identity,
            prepared,
        })
    }

    pub(super) async fn publish_parallel_tool_starts(
        &self,
        claim: &TurnClaim,
        composition: &AgentCompositionPin,
        fold: &mut ContextFold,
        pending: Vec<PendingToolEffect>,
    ) -> std::result::Result<Vec<PreparedToolEffect>, DriveFailure> {
        let mut intents = Vec::with_capacity(pending.len());
        let mut prepared = Vec::with_capacity(pending.len());
        for effect in pending {
            let PendingToolEffect {
                effect_id,
                identity,
                name,
                arguments,
                approval,
                parallel_safe,
                prepared: prepared_call,
            } = effect;
            intents.push(SessionFactBody::ToolIntent {
                turn_id: claim.turn_id().clone(),
                effect_id: effect_id.clone(),
                identity: identity.clone(),
                name,
                arguments,
                approval,
                parallel_safe,
            });
            prepared.push(PreparedToolEffect {
                effect_id,
                identity,
                prepared: prepared_call,
            });
        }

        let intents = self.publish_apply(claim, fold, intents).await?;
        self.flush_last(claim, &intents).await?;
        let starts = prepared
            .iter()
            .map(|effect| SessionFactBody::ToolStarted {
                turn_id: claim.turn_id().clone(),
                effect_id: effect.effect_id.clone(),
                identity: effect.identity.clone(),
            })
            .collect();
        let starts = self.publish_apply(claim, fold, starts).await?;
        self.flush_last(claim, &starts).await?;
        for effect in &prepared {
            self.track_tool(claim, composition.clone(), effect.identity.clone());
        }
        Ok(prepared)
    }

    pub(super) async fn publish_tool_result(
        &self,
        claim: &TurnClaim,
        composition: &AgentCompositionPin,
        fold: &mut ContextFold,
        effect_id: EffectId,
        identity: ToolResultIdentity,
        result: ToolResult,
    ) -> std::result::Result<(), DriveFailure> {
        let returned = self
            .publish_apply(
                claim,
                fold,
                vec![SessionFactBody::ToolResult {
                    turn_id: claim.turn_id().clone(),
                    effect_id,
                    identity: identity.clone(),
                    result,
                }],
            )
            .await
            .map_err(|failure| settled_tool_budget(failure, identity.clone()))?;
        self.flush_last(claim, &returned).await?;
        composition
            .tools()
            .commit(&identity)
            .map_err(|error| tool_failure(&error))?;
        self.clear_tracked_tool(claim, &identity);
        Ok(())
    }

    pub(super) fn track_tool(
        &self,
        claim: &TurnClaim,
        composition: AgentCompositionPin,
        identity: ToolResultIdentity,
    ) {
        self.active_tools
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .entry((claim.session_id().clone(), claim.turn_id().clone()))
            .or_default()
            .insert(identity, TrackedTool { composition });
    }

    pub(super) fn clear_tracked_tool(&self, claim: &TurnClaim, identity: &ToolResultIdentity) {
        let key = (claim.session_id().clone(), claim.turn_id().clone());
        let mut active = self
            .active_tools
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(tools) = active.get_mut(&key) {
            tools.remove(identity);
            if tools.is_empty() {
                active.remove(&key);
            }
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
    }

    #[allow(clippy::too_many_arguments)] // Start binds one prepared effect to its durable identity and exact turn-scoped authorities.
    pub(super) async fn start_tool(
        &self,
        prepared: Box<dyn PreparedToolCall>,
        identity: &ToolResultIdentity,
        composition: &AgentCompositionPin,
        claim: &TurnClaim,
        job_scope: Option<&JobScopeAuthority>,
        scheduling: ToolScheduling,
        sandbox_mode: SandboxMode,
        cancellation: &CancellationToken,
        stop: &CancellationToken,
    ) -> std::result::Result<ToolResult, DriveFailure> {
        let combined = combine_cancellation(cancellation, stop);
        let cwd = std::path::PathBuf::from(claim.header().canonical_cwd());
        let mut extensions = rsi_tools_protocol::ToolExecutionExtensions::default();
        if let Ok(caller) = self.turns.agent_caller(claim) {
            extensions = extensions
                .with(Arc::new(caller))
                .map_err(|error| fatal(error.to_string()))?;
        }
        if scheduling == ToolScheduling::ExclusiveFinal
            && let Ok(parking) = EXECUTOR_LANE_PARKING.try_with(Clone::clone)
        {
            extensions = extensions
                .with(Arc::new(parking))
                .map_err(|error| fatal(error.to_string()))?;
        }
        let result = prepared
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
            .await;
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

    pub(super) async fn request_tool_approval(
        &self,
        claim: &TurnClaim,
        effect_id: &EffectId,
        tool_name: &str,
        required: bool,
        cancellation: &CancellationToken,
        stop: &CancellationToken,
    ) -> std::result::Result<Option<rsi_approval_protocol::ApprovalOutcome>, DriveFailure> {
        if !required {
            return Ok(None);
        }
        let request = ApprovalRequest {
            subject: ApprovalSubject::new(
                claim.session_id().as_str(),
                claim.turn_id().as_str(),
                effect_id.as_str(),
            )
            .map_err(|error| failed("approval.invalid_subject", error.to_string()))?,
            id: effect_id.as_str().to_owned(),
            action: format!("run tool {tool_name}"),
            reason: format!(
                "Agent turn {} requested this Tool effect",
                claim.turn_id().as_str()
            ),
        };
        let outcome = tokio::select! {
            outcome = self.approval.ask(request, cancellation.clone()) => outcome,
            () = stop.cancelled() => return Err(DriveFailure::Stopped),
        };
        match outcome {
            Ok(outcome) if outcome.decision == ApprovalDecision::AllowOnce => Ok(Some(outcome)),
            Ok(_) => Err(failed(
                "approval.denied",
                "live approval denied the Tool effect",
            )),
            Err(ApprovalError::Cancelled) if cancellation.is_cancelled() => {
                Err(DriveFailure::Turn(TurnOutcome::Cancelled))
            }
            Err(error) => Err(failed("approval.failed", error.to_string())),
        }
    }

    pub(super) async fn recover_tool(
        &self,
        claim: &TurnClaim,
        composition: &AgentCompositionPin,
        fold: &mut ContextFold,
        effect_id: EffectId,
        identity: ToolResultIdentity,
        stop: &CancellationToken,
    ) -> std::result::Result<(), DriveFailure> {
        let tools = composition.tools();
        self.track_tool(claim, composition.clone(), identity.clone());
        let retained = tools.wait(&identity, stop.clone()).await;
        if stop.is_cancelled() {
            return Err(DriveFailure::Stopped);
        }
        match retained.map_err(|error| tool_failure(&error))? {
            RetainedToolResult::Returned(result) => {
                let facts = self
                    .publish_apply(
                        claim,
                        fold,
                        vec![SessionFactBody::ToolResult {
                            turn_id: claim.turn_id().clone(),
                            effect_id: effect_id.clone(),
                            identity: identity.clone(),
                            result,
                        }],
                    )
                    .await
                    .map_err(|failure| settled_tool_budget(failure, identity.clone()))?;
                self.flush_last(claim, &facts).await?;
                tools
                    .commit(&identity)
                    .map_err(|error| tool_failure(&error))?;
                self.clear_tracked_tool(claim, &identity);
                Ok(())
            }
            RetainedToolResult::Failed(failure) => {
                let outcome = match failure.kind {
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
                Err(DriveFailure::SettledTool { outcome, identity })
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

    pub(super) async fn record_model_failure(
        &self,
        claim: &TurnClaim,
        fold: &mut ContextFold,
        effect_id: &EffectId,
        error: rsi_ai_protocol::AiError,
    ) -> std::result::Result<(), DriveFailure> {
        let facts = self
            .publish_apply(
                claim,
                fold,
                vec![SessionFactBody::ModelEvent {
                    turn_id: claim.turn_id().clone(),
                    effect_id: effect_id.clone(),
                    event: LanguageEvent::Failed {
                        error,
                        replay: None,
                    },
                }],
            )
            .await?;
        self.flush_last(claim, &facts).await
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
        match tokio::time::timeout(
            self.config.finalization_wait(),
            self.finalization.finalize(&context),
        )
        .await
        {
            Ok(Ok(report)) => match report.completion_blocker() {
                Some(blocker) => {
                    apply_finalization_failure(outcome, blocker.code(), blocker.message(), false)
                }
                None => outcome,
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

    pub(super) async fn finish_budget(
        &self,
        claim: &TurnClaim,
        job_scope: Option<&JobScopeAuthority>,
        dimension: BudgetDimension,
        consumed: u64,
        limit: u64,
    ) -> std::result::Result<Vec<Arc<SessionFact>>, DriveFailure> {
        let outcome = self
            .resolve_outcome(
                claim,
                job_scope,
                TurnOutcome::BudgetExceeded {
                    dimension,
                    consumed,
                    limit,
                },
            )
            .await;
        if !matches!(
            outcome,
            TurnOutcome::BudgetExceeded {
                dimension: outcome_dimension,
                consumed: outcome_consumed,
                limit: outcome_limit,
            } if outcome_dimension == dimension
                && outcome_consumed == consumed
                && outcome_limit == limit
        ) {
            let terminal =
                publish_terminal(self.turns.as_ref(), &self.config, claim, outcome).await?;
            return Ok(vec![terminal]);
        }
        let exhausted = publish_budget_exhaustion(
            self.turns.as_ref(),
            &self.config,
            claim,
            dimension,
            consumed,
            limit,
        )
        .await?;
        let terminal = publish_terminal(self.turns.as_ref(), &self.config, claim, outcome).await?;
        Ok(vec![exhausted, terminal])
    }

    const fn context_limits(&self) -> ContextLimits {
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

    pub(super) fn request_checkpoint(&self, claim: &TurnClaim) {
        let _ignored = self
            .checkpoints
            .schedule(CheckpointRequest::new(claim.clone(), self.context_limits()));
    }

    pub(super) async fn publish_apply(
        &self,
        claim: &TurnClaim,
        fold: &mut ContextFold,
        bodies: Vec<SessionFactBody>,
    ) -> std::result::Result<Vec<Arc<SessionFact>>, DriveFailure> {
        let facts = publish_nonterminal_with_capacity_retry(
            self.turns.as_ref(),
            &self.config,
            claim,
            bodies,
        )
        .await?;
        if facts
            .first()
            .is_some_and(|fact| fact.seq() != fold.through_seq() + 1)
        {
            self.sync_fold(claim, fold).await?;
        }
        if let Some(unapplied) = facts
            .iter()
            .position(|fact| fact.seq() > fold.through_seq())
        {
            fold.apply(&facts[unapplied..])
                .map_err(|error| failed("context.incremental", error.to_string()))?;
        }
        Ok(facts)
    }

    pub(super) async fn flush_last(
        &self,
        claim: &TurnClaim,
        facts: &[Arc<SessionFact>],
    ) -> std::result::Result<(), DriveFailure> {
        let seq = facts
            .last()
            .ok_or_else(|| failed("executor.empty_publish", "publication returned no Facts"))?
            .seq();
        self.flush_durable(claim, seq).await?;
        Ok(())
    }

    pub(super) async fn flush_durable(
        &self,
        claim: &TurnClaim,
        through_seq: u64,
    ) -> std::result::Result<u64, DriveFailure> {
        tokio::time::timeout(
            self.config.durability_wait(),
            self.turns.flush(claim, through_seq),
        )
        .await
        .map_err(|_| {
            fatal(format!(
                "Fact durability wait exceeded {} ms",
                self.config.durability_wait_ms
            ))
        })?
        .map_err(fatal)
    }

    pub(super) async fn sync_fold(
        &self,
        claim: &TurnClaim,
        fold: &mut ContextFold,
    ) -> std::result::Result<(), DriveFailure> {
        loop {
            let after_seq = fold.through_seq();
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
            fold.apply_page(&page.facts, page.through_seq)
                .map_err(|error| failed("context.incremental", error.to_string()))?;
        }
    }

    pub(super) async fn load_claim(
        &self,
        claim: &TurnClaim,
        fold: &mut ContextFold,
    ) -> std::result::Result<ScannedTurn, DriveFailure> {
        let mut state = ScannedTurn::default();
        let mut cursor = 0;
        let mut restored_checkpoint = false;
        if let Ok(Some(checkpoint)) = self.turns.read_context_checkpoint(claim.session_id()).await
            && checkpoint.through_seq < claim.accepted_seq()
            && checkpoint.through_seq <= claim.live_seq()
            && let Ok(restored) = ContextFold::from_checkpoint(
                claim.header().clone(),
                self.context_limits(),
                &checkpoint.bytes,
            )
            && restored.through_seq() == checkpoint.through_seq
            && restored.fact_prefix_sha256() == checkpoint.fact_prefix_sha256
            && claim
                .header()
                .fingerprint()
                .is_ok_and(|fingerprint| fingerprint == checkpoint.header_fingerprint)
        {
            *fold = restored;
            cursor = checkpoint.through_seq;
            restored_checkpoint = true;
        }
        if !restored_checkpoint {
            self.load_fork_seed(claim, fold).await?;
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
            if page.through_seq == cursor {
                return Ok(state);
            }
            cursor = page.through_seq;
            fold.apply_page(&page.facts, page.through_seq)
                .map_err(|error| failed("context.invalid", error.to_string()))?;
            scan_turn(claim, &mut state, &page.facts)
                .map_err(|message| failed("executor.invalid_history", message))?;
        }
    }

    pub(super) async fn load_fork_seed(
        &self,
        claim: &TurnClaim,
        fold: &mut ContextFold,
    ) -> std::result::Result<(), DriveFailure> {
        let Some(origin) = claim.header().fork_origin() else {
            return Ok(());
        };
        let mut cursor = origin.resolved_after_seq;
        loop {
            let page = self
                .turns
                .read_fork_facts(
                    claim,
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
            if page.through_parent_seq == cursor {
                if cursor != page.terminal_parent_seq {
                    return Err(failed(
                        "context.fork_stalled",
                        "fork history reader made no progress",
                    ));
                }
                break;
            }
            fold.apply_seed_page(&page.facts)
                .map_err(|error| failed("context.invalid_fork", error.to_string()))?;
            cursor = page.through_parent_seq;
        }
        fold.finish_seed()
            .map_err(|error| failed("context.invalid_fork", error.to_string()))
    }
}
