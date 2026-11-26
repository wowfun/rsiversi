//! Idle compaction competes with submissions for the exact Session admission key.
use super::*;
use rsi_agent_session_protocol::{DomainRequestId, ManualCompactionEvent};
use rsi_agent_turn_protocol::{IdleCompaction, ManualCompactionAdmission};

struct Reservation {
    guard: SubmissionGuard,
    cancellation: CancellationToken,
}

impl Drop for Reservation {
    fn drop(&mut self) {
        self.guard
            .key
            .maintenance
            .lock()
            .expect("maintenance lock")
            .take();
    }
}

struct Idle {
    kernel: AgentKernel,
    reservation: Arc<Reservation>,
    header: SessionHeader,
    command: DomainRequestId,
    composition: AgentCompositionPin,
    through: u64,
    effect: Option<EffectId>,
}

pub(super) async fn reserve(
    kernel: &AgentKernel,
    id: &SessionId,
    command: &DomainRequestId,
) -> TurnResult<ManualCompactionAdmission> {
    let key = kernel.inner.submission_admission.register(id);
    let Ok(guard) = key.mutex.clone().try_lock_owned() else {
        return Ok(ManualCompactionAdmission::Busy);
    };
    if key.maintenance.lock().expect("maintenance lock").is_some() {
        return Ok(ManualCompactionAdmission::Busy);
    }
    {
        let state = lock_state(&kernel.inner);
        if !state.accepting {
            return Err(TurnError::ShuttingDown);
        }
        if state.loading_sessions.contains_key(id)
            || state.fresh_reservations.contains(id)
            || state
                .sessions
                .get(id)
                .is_some_and(|s| s.turns.values().any(|t| t.terminal.is_none()))
        {
            return Ok(ManualCompactionAdmission::Busy);
        }
    }
    let cancellation = kernel.inner.submission_admission.closed.child_token();
    *key.maintenance.lock().expect("maintenance lock") =
        Some(MaintenanceOwner::Compaction(cancellation.clone()));
    let reservation = Arc::new(Reservation {
        guard: SubmissionGuard { _guard: guard, key },
        cancellation,
    });
    kernel.flush_resident_admitted(id).await?;
    let state = kernel
        .inner
        .store
        .read_inbox_control_state(id, false)
        .await
        .map_err(turn_store_error)?;
    if state.has_open_turn || state.has_active_activation || state.has_waking_message {
        return Ok(ManualCompactionAdmission::Busy);
    }
    if let Some(fact) = kernel
        .inner
        .store
        .manual_compaction_fact(id, command)
        .await
        .map_err(turn_store_error)?
    {
        let outcome =
            rsi_agent_turn_protocol::CompactionOutcome::recorded(&fact).ok_or_else(|| {
                TurnError::Invariant("store returned a different command stage".into())
            })?;
        return Ok(ManualCompactionAdmission::Recorded(outcome));
    }
    let prepared = kernel.prepare_resume_session(id).await?;
    let (header, composition) = kernel.inner.resume_issuer.consume(prepared)?;
    Ok(ManualCompactionAdmission::Idle(Box::new(Idle {
        kernel: kernel.clone(),
        reservation,
        header,
        command: command.clone(),
        composition,
        through: state.durable_fact_seq,
        effect: None,
    })))
}

impl fmt::Debug for Idle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("IdleCompaction")
            .field("session", self.header.session_id())
            .field("through", &self.through)
            .finish_non_exhaustive()
    }
}

fn context_error(error: rsi_agent_context::ContextError) -> TurnError {
    match error {
        rsi_agent_context::ContextError::Capacity => TurnError::Capacity,
        other => TurnError::Invalid(other.to_string()),
    }
}

impl Idle {
    async fn replay(
        &self,
        fold: &mut rsi_agent_context::ModelContextState,
        session: &SessionId,
        mut after: u64,
        through: u64,
        inherited: bool,
    ) -> TurnResult<()> {
        use rsi_agent_context::ContextPage;
        while after < through {
            if self.reservation.cancellation.is_cancelled() {
                return Err(TurnError::ShuttingDown);
            }
            let limit = usize::try_from((through - after).min(64)).expect("bounded page");
            let (page, _) = read_facts_bounded(&self.kernel.inner, session, after, limit)
                .await
                .map_err(turn_store_error)?;
            after = page
                .facts
                .last()
                .ok_or_else(|| TurnError::Invariant("missing compaction history".into()))?
                .seq();
            let facts: Vec<_> = page.facts.into_iter().map(Arc::new).collect();
            fold.ingest(if inherited {
                ContextPage::ForkSeed(&facts)
            } else {
                ContextPage::Canonical(&facts)
            })
            .map_err(context_error)?;
        }
        Ok(())
    }
}

#[async_trait]
impl IdleCompaction for Idle {
    async fn context(
        &self,
        limits: rsi_agent_context::ContextLimits,
        budget: rsi_agent_context::ContextBudget,
    ) -> TurnResult<rsi_agent_context::ModelContextState> {
        use rsi_agent_context::{ContextPage, ModelContextState};
        let mut fold = ModelContextState::open(
            self.composition.context_builder(),
            self.header.clone(),
            limits,
            budget.clone(),
        )
        .map_err(context_error)?;
        let session = self.header.session_id();
        let checkpoint = match self
            .kernel
            .read_context_checkpoint(session, budget.checkpoint_read())
            .await
        {
            Err(TurnError::Capacity) => return Err(TurnError::Capacity),
            result => result.ok().flatten(),
        };
        if let Some(checkpoint) = checkpoint
            && checkpoint.through_seq <= self.through
            && let Some(mut restored) = checkpoint.restore_for(&fold).map_err(context_error)?
        {
            self.replay(
                &mut restored,
                session,
                checkpoint.through_seq,
                self.through,
                false,
            )
            .await?;
            return Ok(restored);
        }
        if self.header.history_origin().is_some() {
            let intervals = fork_history::capture(&self.kernel.inner, &self.header).await?;
            for interval in &intervals {
                let origin = interval
                    .source
                    .history_origin()
                    .expect("validated inherited interval");
                fold.ingest(ContextPage::ForkSource(&interval.source))
                    .map_err(context_error)?;
                self.replay(
                    &mut fold,
                    &origin.parent_session_id,
                    origin.resolved_after_seq,
                    origin.resolved_terminal_seq,
                    true,
                )
                .await?;
            }
            fold.ingest(ContextPage::FinishSeed)
                .map_err(context_error)?;
        }
        self.replay(&mut fold, self.header.session_id(), 0, self.through, false)
            .await?;
        Ok(fold)
    }

    fn composition(&self) -> &AgentCompositionPin {
        &self.composition
    }
    fn cancellation(&self) -> CancellationToken {
        self.reservation.cancellation.clone()
    }

    async fn start(
        &mut self,
        effect: &EffectId,
        prepared: rsi_agent_session_protocol::PreparedManualCompaction,
    ) -> TurnResult<Vec<Arc<SessionFact>>> {
        if self.effect.is_some()
            || &prepared.generation != self.composition.selection()
            || &prepared.plan.session != self.header.session_id()
            || prepared.plan.through_seq != self.through
        {
            return Err(TurnError::Invalid(
                "manual compaction differs from its idle cut".into(),
            ));
        }
        let now = self.kernel.inner.clock.now_ms().max(1);
        let facts = vec![
            self.fact(
                1,
                now,
                effect,
                ManualCompactionEvent::Prepared(Box::new(prepared)),
            )?,
            self.fact(2, now, effect, ManualCompactionEvent::Started)?,
        ];
        let committed = self.commit(facts).await?;
        self.effect = Some(effect.clone());
        Ok(committed)
    }

    async fn settle(
        mut self: Box<Self>,
        attempt: rsi_agent_session_protocol::ModelAttemptLog,
        mut fold: rsi_agent_context::ModelContextState,
    ) -> TurnResult<rsi_agent_turn_protocol::CompactionOutcome> {
        use rsi_agent_turn_protocol::CompactionOutcome;
        let effect = self
            .effect
            .take()
            .ok_or_else(|| TurnError::Invariant("manual compaction never started".into()))?;
        let now = self.kernel.inner.clock.now_ms().max(1);
        let settled = self.fact(1, now, &effect, ManualCompactionEvent::Settled { attempt })?;
        let mut facts = vec![settled];
        let applied = fold.ingest(rsi_agent_context::ContextPage::Canonical(&facts));
        let installed = applied.is_ok() && fold.summary_installed(&effect);
        if installed {
            facts.push(self.fact(2, now, &effect, ManualCompactionEvent::Installed)?);
        }
        let outcome = CompactionOutcome::recorded(facts.last().expect("settled attempt"))
            .expect("command fact");
        self.commit(facts).await?;
        applied.map_err(context_error)?;
        Ok(outcome)
    }
}

impl Idle {
    fn fact(
        &self,
        offset: u64,
        timestamp: u64,
        effect: &EffectId,
        event: ManualCompactionEvent,
    ) -> TurnResult<Arc<SessionFact>> {
        let seq = self
            .through
            .checked_add(offset)
            .ok_or(TurnError::Capacity)?;
        SessionFact::new(
            seq,
            timestamp,
            SessionFactBody::ManualCompaction {
                command_id: self.command.clone(),
                effect_id: effect.clone(),
                event,
            },
        )
        .map(Arc::new)
        .map_err(|error| TurnError::Invalid(error.to_string()))
    }
    async fn commit(&mut self, facts: Vec<Arc<SessionFact>>) -> TurnResult<Vec<Arc<SessionFact>>> {
        let id = self.header.session_id().clone();
        let expected = self.through;
        let kernel = self.kernel.clone();
        let guard = Arc::clone(&self.reservation);
        let committed = self
            .kernel
            .owned_commit(async move {
                let _guard = guard;
                let receipt = kernel
                    .inner
                    .store
                    .append(AppendBatch {
                        session_id: id.clone(),
                        expected_seq: expected,
                        header: None,
                        facts: facts.clone(),
                    })
                    .await
                    .map_err(turn_store_error)?;
                {
                    let mut state = lock_state(&kernel.inner);
                    if let Some(session) = state.sessions.get_mut(&id) {
                        session.durable_seq = receipt.durable_seq;
                        session.flush_status.send_replace(FlushStatus {
                            durable_seq: receipt.durable_seq,
                            permanent_error: session.permanent_flush_error.clone(),
                        });
                        publish_live_watermarks(session);
                    }
                }
                kernel.inner.session_changes.committed(&id);
                Ok(facts)
            })
            .await?;
        self.through = committed.last().expect("admitted nonempty prefix").seq();
        Ok(committed)
    }
}
