//! Explicit summaries under an idle reservation or the currently executing Turn claim.
use super::*;
use rsi_agent_session_protocol::{
    CompactionTrigger, ModelAttemptBuilder, ModelEventPurpose, ModelPurpose, ModelSelection,
    PreparedManualCompaction,
};
use rsi_agent_turn_protocol::{
    CompactionOutcome, CompactionRegion, IdleCompaction, ManualCompactor, Result as TurnResult,
};

fn invalid(error: impl fmt::Display) -> TurnError {
    TurnError::Invalid(bounded(&error.to_string()))
}

fn drive_error(error: DriveFailure) -> TurnError {
    match error {
        DriveFailure::Stopped => TurnError::ShuttingDown,
        DriveFailure::Fatal(message) => TurnError::Invariant(message),
        DriveFailure::Turn(TurnOutcome::Failed { message, .. }) => invalid(message),
        other => invalid(format!("{other:?}")),
    }
}

impl RunningTurn {
    fn region_claim(&self) -> TurnResult<TurnClaim> {
        if self
            .region
            .as_ref()
            .is_some_and(|work| !work.done.is_cancelled())
        {
            return Err(invalid("a region compaction is already running"));
        }
        self.claim.clone().ok_or_else(|| invalid("Turn is closing"))
    }
}

#[async_trait]
impl ManualCompactor for Driver {
    async fn install_plugin_checkpoint(
        &self,
        session: &SessionId,
        turn: &TurnId,
        effect: EffectId,
        mut checkpoint: rsi_agent_session_protocol::PluginCheckpoint,
    ) -> TurnResult<()> {
        checkpoint.validate().map_err(invalid)?;
        let claim = self
            .controlled_work
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&(session.clone(), turn.clone()))
            .ok_or_else(|| invalid("plugin checkpoint requires its open Turn"))?
            .region_claim()?;
        if checkpoint.through_seq < claim.accepted_seq() {
            return Err(invalid("plugin checkpoint precedes its Turn"));
        }
        let composition = self.turns.composition(&claim)?;
        let mut fold = ModelContextState::open(
            composition.context_builder(),
            claim.header().clone(),
            self.context_limits(),
            self.context_budget.clone(),
        )
        .map_err(invalid)?;
        self.load_claim_measured(
            &claim,
            &mut fold,
            &mut checkpoint_diagnostics::Scan::default(),
            Some(checkpoint.through_seq),
        )
        .await
        .map_err(drive_error)?;
        if fold.position().through_seq != checkpoint.through_seq {
            return Err(invalid(
                "plugin checkpoint prefix is beyond the current Session",
            ));
        }
        let observed = fold
            .plugin_region_view(checkpoint.start, checkpoint.end)
            .map_err(invalid)?;
        self.sync_fold(&claim, &mut fold)
            .await
            .map_err(drive_error)?;
        let current = fold.position().through_seq;
        if current != checkpoint.through_seq
            && fold
                .plugin_region_view(checkpoint.start, checkpoint.end)
                .map_err(invalid)?
                != observed
        {
            return Err(invalid(
                "conversation changed before checkpoint installation",
            ));
        }
        checkpoint.through_seq = current;
        let next = current
            .checked_add(1)
            .ok_or_else(|| invalid("Session sequence exhausted"))?;
        // Preflight the largest timestamp encoding. Kernel stamps the actual publication.
        let fact = Arc::new(
            SessionFact::new(
                next,
                u64::MAX,
                SessionFactBody::PluginContextCompacted {
                    turn_id: turn.clone(),
                    effect_id: effect.clone(),
                    view_sha256: observed,
                    checkpoint: Box::new(checkpoint),
                },
            )
            .map_err(invalid)?,
        );
        fold.ingest(ContextPage::Canonical(std::slice::from_ref(&fact)))
            .map_err(invalid)?;
        if !fold.summary_installed(&effect) {
            return Err(invalid("context builder declined the plugin checkpoint"));
        }
        drop(fold);
        let body = Arc::unwrap_or_clone(fact).into_body();
        let receipt = publish_nonterminal_with_capacity_retry(
            self.turns.as_ref(),
            &self.config,
            &claim,
            vec![body],
        )
        .await?;
        self.flush_durable(&claim, receipt.facts()[0].seq())
            .await
            .map_err(|_| TurnError::ExecutionOutcomeUnknown)?;
        Ok(())
    }
    async fn compact(
        &self,
        mut lease: Box<dyn IdleCompaction>,
        model: ModelSelection,
    ) -> TurnResult<CompactionOutcome> {
        let cancel = lease.cancellation();
        let limits = ContextLimits::new(
            self.config.max_context_messages,
            self.config.max_context_bytes,
        )
        .map_err(invalid)?;
        let mut fold = tokio::select! { biased;
            () = cancel.cancelled() => return Ok(CompactionOutcome::Interrupted),
            () = self.stopped.cancelled() => return Ok(CompactionOutcome::Interrupted),
            result = lease.context(limits, self.context_budget.clone()) => result?,
        };
        let profile = self
            .language
            .describe(&model.model)
            .map_err(invalid)?
            .into_profile();
        let cap = fold
            .header()
            .settings()
            .output_budgets()
            .cap(&model.model)
            .or(profile.default_output_tokens());
        let (options, _credit) = compaction::admitted_options(
            &self.context_budget,
            lease.composition(),
            &model,
            cap,
            None,
        )
        .map_err(|outcome| drive_error(DriveFailure::Turn(outcome)))?;
        let Some(planned) = fold
            .plan_compaction(
                &options,
                &model.model,
                &profile,
                Some(CompactionTrigger::Manual),
                false,
            )
            .map_err(invalid)?
        else {
            return Ok(CompactionOutcome::NoChange);
        };
        if self.is_archived(fold.header()).await? {
            return Err(invalid(
                "Restore the archived Session before compacting its history.",
            ));
        }
        let capture = evidence::Capture::new(&planned.request, &profile).map_err(drive_error)?;
        let prepared = tokio::select! { biased;
            () = cancel.cancelled() => return Ok(CompactionOutcome::Interrupted),
            () = self.stopped.cancelled() => return Ok(CompactionOutcome::Interrupted),
            result = self.language.prepare(model.model, planned.request) => result.map_err(invalid)?,
        };
        let snapshot = prepared.snapshot().clone();
        let evidence = capture.finish(&snapshot).map_err(drive_error)?;
        let quote = fold
            .header()
            .settings()
            .pricing()
            .resolve(&snapshot)
            .cloned();
        let effect = next_effect_id().map_err(invalid)?;
        let generation = lease.composition().selection().clone();
        let prefix = lease
            .start(
                &effect,
                PreparedManualCompaction {
                    generation,
                    plan: planned.plan,
                    snapshot,
                    price_quote: quote,
                    evidence,
                },
            )
            .await?;
        fold.ingest(ContextPage::Canonical(&prefix))
            .map_err(invalid)?;
        drop(prefix);
        let attempt = self.manual_attempt(prepared, &cancel).await?;
        lease.settle(attempt, fold).await
    }

    async fn compact_region(
        self: Arc<Self>,
        session: &SessionId,
        region: CompactionRegion,
        model: ModelSelection,
    ) -> TurnResult<EffectId> {
        let (claim, tracker, cancel, settled) = {
            let mut turns = self
                .controlled_work
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let running = turns
                .get_mut(&(session.clone(), region.turn_id.clone()))
                .ok_or_else(|| invalid("region compaction requires the current open Turn"))?;
            let claim = running.region_claim()?;
            let cancel = self.turns.cancellation(&claim)?.child_token();
            let done = CancellationToken::new();
            running.region = Some(RegionWork {
                cancel: cancel.clone(),
                done: done.clone(),
            });
            (claim, Arc::clone(&running.work), cancel, done.drop_guard())
        };
        let _caller = cancel.clone().drop_guard();
        if region.start > region.end || region.through_seq < claim.accepted_seq() {
            return Err(invalid("invalid compaction region or Turn boundary"));
        }
        let composition = self.turns.composition(&claim)?;
        let mut fold = ModelContextState::open(
            composition.context_builder(),
            claim.header().clone(),
            self.context_limits(),
            self.context_budget.clone(),
        )
        .map_err(invalid)?;
        self.load_claim_measured(
            &claim,
            &mut fold,
            &mut checkpoint_diagnostics::Scan::default(),
            Some(region.through_seq),
        )
        .await
        .map_err(drive_error)?;
        if fold.position().through_seq != region.through_seq {
            return Err(invalid("compaction prefix is beyond the current Session"));
        }
        let options = self
            .prepare_request_options(&claim, &composition, &model, Some(&[]))
            .map_err(drive_error)?;
        let plan = |fold: &ModelContextState| {
            fold.plan_compaction(
                &options.options,
                &model.model,
                &options.profile,
                Some(CompactionTrigger::Region {
                    start: region.start,
                    end: region.end,
                }),
                false,
            )
            .map_err(invalid)?
            .ok_or_else(|| invalid("selected region cannot be compacted"))
        };
        let mut planned = plan(&fold)?;
        self.sync_fold(&claim, &mut fold)
            .await
            .map_err(drive_error)?;
        if fold.position().through_seq != region.through_seq {
            let original = planned.plan;
            drop(planned.request);
            planned = plan(&fold)?;
            if original.view_sha256 != planned.plan.view_sha256
                || original.prior != planned.plan.prior
                || original.selections != planned.plan.selections
            {
                return Err(invalid("conversation changed before region compaction"));
            }
        }
        let execution = admit_claim_execution(&claim).map_err(|error| drive_error(*error))?;
        if self.is_archived(claim.header()).await? {
            return Err(invalid(
                "Restore the archived Session before compacting its history.",
            ));
        }
        let capture =
            evidence::Capture::new(&planned.request, &options.profile).map_err(drive_error)?;
        let prepared = tokio::select! { biased;
            () = cancel.cancelled() => return Err(invalid("Turn cancelled")),
            () = self.stopped.cancelled() => return Err(TurnError::ShuttingDown),
            result = self.language.prepare(model.model, planned.request) => result.map_err(invalid)?,
        };
        let snapshot = prepared.snapshot().clone();
        let evidence = capture.finish(&snapshot).map_err(drive_error)?;
        let price_quote = claim
            .header()
            .settings()
            .pricing()
            .resolve(&snapshot)
            .cloned();
        let effect = next_effect_id().map_err(invalid)?;
        // Once the intent may commit, losing the RPC waiter must still leave a
        // settled provider attempt. The Turn joins this worker before release.
        tokio::spawn(async move {
            let _settled = settled;
            let _execution = execution;
            if cancel.is_cancelled() {
                return Err(invalid("region compaction cancelled"));
            }
            self.publish_durable(
                &claim,
                &mut fold,
                vec![
                    SessionFactBody::ModelIntent {
                        turn_id: region.turn_id.clone(),
                        effect_id: effect.clone(),
                        purpose: ModelPurpose::ContextCompaction(Box::new(planned.plan)),
                        snapshot,
                        evidence,
                        price_quote,
                    },
                    SessionFactBody::ModelStarted {
                        turn_id: region.turn_id.clone(),
                        effect_id: effect.clone(),
                    },
                ],
            )
            .await
            .map_err(drive_error)?;
            let work = tracker.guard();
            let attempt = self.manual_attempt(prepared, &cancel).await?;
            let receipt = self
                .publish_durable(
                    &claim,
                    &mut fold,
                    vec![SessionFactBody::ModelAttemptSettled {
                        turn_id: region.turn_id.clone(),
                        effect_id: effect.clone(),
                        purpose: ModelEventPurpose::ContextCompaction,
                        attempt,
                    }],
                )
                .await
                .map_err(drive_error)?;
            work.confirm();
            if !fold.summary_installed(&effect) {
                return Err(invalid(
                    "summary did not install against the selected conversation",
                ));
            }
            self.publish_durable(
                &claim,
                &mut fold,
                vec![SessionFactBody::ContextCompactionInstalled {
                    turn_id: region.turn_id,
                    effect_id: effect.clone(),
                    settlement_seq: receipt.facts()[0].seq(),
                }],
            )
            .await
            .map_err(drive_error)?;
            Ok(effect)
        })
        .await
        .map_err(|error| TurnError::Invariant(error.to_string()))?
    }
}

impl Driver {
    pub(super) async fn close_region(&self, claim: &TurnClaim) {
        let region = self
            .controlled_work
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get_mut(&(claim.session_id().clone(), claim.turn_id().clone()))
            .and_then(|running| {
                running.claim = None;
                running.region.take()
            });
        if let Some(region) = region {
            region.cancel.cancel();
            region.done.cancelled().await;
        }
    }

    async fn manual_attempt(
        &self,
        prepared: Box<dyn rsi_ai_protocol::PreparedLanguageCall>,
        cancel: &CancellationToken,
    ) -> TurnResult<rsi_agent_session_protocol::ModelAttemptLog> {
        let started = tokio::time::Instant::now();
        let call = cancel.child_token();
        let _release = call.clone().drop_guard();
        let stream = tokio::select! { biased;
            () = cancel.cancelled() => Err(driver::model_cancelled()),
            () = self.stopped.cancelled() => Err(driver::model_cancelled()),
            result = prepared.start(call) => result,
        };
        let mut stream = match stream {
            Ok(stream) => stream,
            Err(error) => futures_util::stream::once(async move { Err(error) }).boxed(),
        };
        let mut assembler = LanguageAssembler::new();
        let mut events = ModelAttemptBuilder::new();
        loop {
            let next = tokio::select! { biased;
                () = cancel.cancelled() => Err(driver::model_cancelled()),
                () = self.stopped.cancelled() => Err(driver::model_cancelled()),
                next = stream.next() => next.unwrap_or_else(|| Err(driver::model_stream_error("Language stream ended without a terminal event"))),
            };
            let elapsed = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
            let (event, _) = driver::record_event(&mut assembler, &mut events, elapsed, next)
                .map_err(TurnError::Invariant)?;
            if matches!(
                event,
                LanguageEvent::Failed { .. } | LanguageEvent::Finished { .. }
            ) {
                break;
            }
        }
        drop(stream);
        events.finish(true).map_err(invalid)
    }
}
