//! Explicit paid summary under an idle kernel claim; recovery only observes its facts.
use super::*;
use rsi_agent_session_protocol::{
    CompactionTrigger, ModelAttemptBuilder, ModelSelection, PreparedManualCompaction,
};
use rsi_agent_turn_protocol::{
    CompactionOutcome, IdleCompaction, ManualCompactor, Result as TurnResult,
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

#[async_trait]
impl ManualCompactor for Driver {
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
}

impl Driver {
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
