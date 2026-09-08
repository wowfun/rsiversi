use super::*;

impl AgentKernel {
    pub(super) async fn reconcile_waiting_activations(&self) -> Result<()> {
        let mut after = None;
        loop {
            let page = self
                .inner
                .store
                .list_waiting_activations(after.as_ref(), MAXIMUM_SESSIONS_PER_READ)
                .await?;
            page.validate()?;
            let has_more = page.has_more;
            let next = page.sessions.last().cloned();
            for session_id in page.sessions {
                self.settle_waiting_ancestors(session_id)
                    .await
                    .map_err(kernel_turn_error)?;
            }
            if !has_more {
                return Ok(());
            }
            after = Some(next.ok_or_else(|| {
                KernelError::Invariant("waiting-activation enumeration made no progress".into())
            })?);
        }
    }

    async fn cancel_open_descendant_turns(&self, session_id: &SessionId) -> TurnResult<()> {
        let mut cursor = 0;
        let mut horizon = None;
        let mut failure = None;
        loop {
            let page = self
                .inner
                .store
                .list_open_turns(session_id, cursor, MAXIMUM_FACTS_PER_READ)
                .await
                .map_err(turn_store_error)?;
            page.validate().map_err(turn_store_error)?;
            let horizon = *horizon.get_or_insert(page.durable_seq);
            for turn in page
                .turns
                .iter()
                .take_while(|turn| turn.accepted_seq <= horizon)
            {
                cursor = turn.accepted_seq;
                if let Err(error) = self.cancel(session_id, &turn.turn_id, None).await {
                    failure.get_or_insert(error);
                }
            }
            if !page.has_more
                || page
                    .turns
                    .last()
                    .is_some_and(|turn| turn.accepted_seq >= horizon)
            {
                return failure.map_or(Ok(()), Err);
            }
        }
    }

    #[allow(clippy::too_many_lines)] // Turn terminal, descendant cascade, and activation settlement are one guarded commit protocol.
    pub(super) async fn finish_activation_claim(
        &self,
        claim: &TurnClaim,
        proposed_outcome: &TurnOutcome,
    ) -> TurnResult<Option<Arc<SessionFact>>> {
        let activation_id = {
            let state = lock_state(&self.inner);
            self.validate_claim(&state, claim)?.activation_id.clone()
        };
        let Some(activation_id) = activation_id else {
            return Ok(None);
        };
        let drain = self.drain_agent_mutations(claim).await?;
        self.close_current_step(claim, proposed_outcome).await?;
        if !matches!(proposed_outcome, TurnOutcome::Completed) {
            let descendants = descendant_session_ids(&self.inner.store, claim.session_id()).await?;
            let cancellations = descendants.iter().map(|child_session_id| async move {
                let result = self.cancel_open_descendant_turns(child_session_id).await;
                (child_session_id.clone(), result)
            });
            let deadline = Instant::now() + DURABILITY_WAIT_TIMEOUT;
            let results =
                tokio::time::timeout_at(deadline, futures_util::future::join_all(cancellations))
                    .await
                    .map_err(|_| {
                        TurnError::Flush(format!(
                            "descendant cancellation exceeded the cumulative {} second deadline",
                            DURABILITY_WAIT_TIMEOUT.as_secs()
                        ))
                    })?;
            let mut failures = results
                .into_iter()
                .filter_map(|(session_id, result)| {
                    result
                        .err()
                        .map(|error| format!("{}: {error}", session_id.as_str()))
                })
                .collect::<Vec<_>>();
            if !failures.is_empty() {
                let count = failures.len();
                let first = failures.remove(0);
                return Err(TurnError::Store(format!(
                    "{count} descendant cancellation(s) failed; first failure: {first}"
                )));
            }
        }

        let parent_session_id = claim
            .header()
            .fork_origin()
            .map(|origin| origin.parent_session_id.clone());
        let admissions = self
            .inner
            .submission_admission
            .acquire_many(
                std::iter::once(claim.session_id().clone())
                    .chain(parent_session_id.iter().cloned()),
            )
            .await?;
        let live_seq = {
            let state = lock_state(&self.inner);
            state
                .sessions
                .get(claim.session_id())
                .ok_or(TurnError::StaleClaim)?
                .live_seq()
                .map_err(turn_kernel_error)?
        };
        self.flush(claim, live_seq).await?;
        let (expected_fact_seq, original) = {
            let state = lock_state(&self.inner);
            let turn = self.validate_claim(&state, claim)?;
            let session = state
                .sessions
                .get(claim.session_id())
                .expect("validated claim session exists");
            if !session.pending.is_empty() || session.durable_seq != live_seq {
                return Err(TurnError::Invariant(
                    "activation terminal flush did not close its speculative suffix".into(),
                ));
            }
            (session.durable_seq, clone_turn_control(turn))
        };
        let active = self
            .inner
            .store
            .active_activation(claim.session_id())
            .await
            .map_err(turn_store_error)?
            .ok_or_else(|| TurnError::Invariant("activation index lost a live claim".into()))?;
        if active.activation_id != activation_id
            || active.phase != StoreActivationPhase::Running
            || active.turn_id.as_ref() != Some(claim.turn_id())
        {
            return Err(TurnError::Invariant(
                "activation index disagrees with the live claim".into(),
            ));
        }
        let terminal_body = canonicalize_terminal(
            SessionFactBody::TurnTerminal {
                turn_id: claim.turn_id().clone(),
                outcome: proposed_outcome.clone(),
            },
            original.cancel_requested,
        );
        let outcome = match &terminal_body {
            SessionFactBody::TurnTerminal { outcome, .. } => outcome.clone(),
            _ => unreachable!("terminal canonicalization preserves its body kind"),
        };
        let terminal = SessionFact::new(
            expected_fact_seq
                .checked_add(1)
                .ok_or_else(|| TurnError::Invariant("Fact sequence exhausted".into()))?,
            self.inner.clock.now_ms().max(1),
            terminal_body,
        )
        .map_err(|error| TurnError::Invalid(error.to_string()))?;
        let budget_original = clone_turn_control(&original);
        let mut staged = original;
        apply_executor_body(&mut staged, terminal.body())?;
        enforce_turn_budget(
            claim.header().settings().turn_budget(),
            &budget_original,
            std::slice::from_ref(&terminal),
            terminal.timestamp_ms(),
        )?;

        let mailbox = self
            .inner
            .store
            .read_agent_mailbox_summary(claim.session_id())
            .await
            .map_err(turn_store_error)?;
        if mailbox.durable_fact_seq != expected_fact_seq {
            return Err(TurnError::Invariant(
                "mailbox summary changed across activation terminal preparation".into(),
            ));
        }
        let expected_control_seq = mailbox.durable_control_seq;

        let activation_outcome = activation_outcome(&outcome);
        let settled_controls = activation_terminal_controls(
            expected_control_seq,
            terminal.timestamp_ms(),
            AgentControlRecordBody::ActivationSettled {
                activation_id: activation_id.clone(),
                outcome: activation_outcome,
            },
            &mailbox.pending_promotable_message_ids,
        )?;
        let terminal = Arc::new(terminal);
        let mut sessions = vec![AtomicSessionAppend {
            session_id: claim.session_id().clone(),
            expected_fact_seq,
            expected_control_seq,
            header: None,
            facts: vec![terminal.clone()],
            controls: settled_controls,
        }];
        if let Some(parent_session_id) = &parent_session_id {
            sessions.push(
                self.completion_append(
                    claim.session_id(),
                    &activation_id,
                    parent_session_id,
                    &outcome,
                    terminal.timestamp_ms(),
                )
                .await?,
            );
        }
        let terminal_lease = self.retain_terminal_mutation(claim)?;
        let kernel = self.clone();
        let claim = claim.clone();
        self.owned_commit(async move {
            drain.admit();
            let _terminal_lease = terminal_lease;
            let settlement = kernel
                .commit_agent_with_flush_conflict_retry(AtomicAgentCommit {
                    sessions,
                    required_active_activations: vec![AgentActivationGuard {
                        session_id: claim.session_id().clone(),
                        activation_id: activation_id.clone(),
                    }],
                    quiescent_descendants_of: Some(claim.session_id().clone()),
                })
                .await?;
            let settled = match settlement {
                Ok(_) => true,
                Err(StoreError::SessionNotQuiescent { .. }) => {
                    let waiting = activation_terminal_controls(
                        expected_control_seq,
                        terminal.timestamp_ms(),
                        AgentControlRecordBody::ActivationWaitingForDescendants {
                            activation_id: activation_id.clone(),
                        },
                        &mailbox.pending_promotable_message_ids,
                    )?;
                    kernel
                        .inner
                        .commit_agent(AtomicAgentCommit {
                            sessions: vec![AtomicSessionAppend {
                                session_id: claim.session_id().clone(),
                                expected_fact_seq,
                                expected_control_seq,
                                header: None,
                                facts: vec![terminal.clone()],
                                controls: waiting,
                            }],
                            required_active_activations: vec![AgentActivationGuard {
                                session_id: claim.session_id().clone(),
                                activation_id: activation_id.clone(),
                            }],
                            quiescent_descendants_of: None,
                        })
                        .await
                        .map_err(turn_store_error)?;
                    false
                }
                Err(error) => return Err(turn_store_error(error)),
            };
            kernel.install_committed_activation_terminal(
                &claim,
                expected_fact_seq,
                terminal.seq(),
            )?;
            drop(admissions);
            kernel.request_ready_scan();
            if settled {
                kernel.inner.settlement_requested.notify_one();
            }
            Ok(Some(terminal))
        })
        .await
    }

    pub(super) async fn completion_append(
        &self,
        child_session_id: &SessionId,
        activation_id: &rsi_agent_session_protocol::ActivationId,
        parent_session_id: &SessionId,
        outcome: &TurnOutcome,
        timestamp_ms: u64,
    ) -> TurnResult<AtomicSessionAppend> {
        let mailbox = self
            .inner
            .store
            .read_agent_mailbox_summary(parent_session_id)
            .await
            .map_err(turn_store_error)?;
        if mailbox.pending_count >= MAXIMUM_PENDING_AGENT_MESSAGES {
            return Err(TurnError::Invariant(
                "reserved completion found a full parent mailbox".into(),
            ));
        }
        let expected_fact_seq = mailbox.durable_fact_seq;
        let open = self
            .inner
            .store
            .list_open_turns(parent_session_id, 0, 1)
            .await
            .map_err(turn_store_error)?;
        let parent_has_step = if let Some(turn) = open.turns.first() {
            self.inner
                .store
                .active_activation(parent_session_id)
                .await
                .map_err(turn_store_error)?
                .is_some_and(|active| {
                    active.turn_id.as_ref() == Some(&turn.turn_id)
                        && matches!(
                            active.phase,
                            StoreActivationPhase::Running | StoreActivationPhase::Parked
                        )
                })
        } else {
            false
        };
        let parent_header = read_validated_header_bounded(&self.inner, parent_session_id)
            .await
            .map_err(turn_store_error)?;
        let message_id = completion_message_id(child_session_id, activation_id)?;
        let control = AgentControlRecord::new(
            mailbox
                .durable_control_seq
                .checked_add(1)
                .ok_or_else(|| TurnError::Invariant("control sequence exhausted".into()))?,
            timestamp_ms,
            AgentControlRecordBody::MessageAccepted {
                delivery: if parent_has_step {
                    rsi_agent_session_protocol::MessageDelivery::NextStep
                } else {
                    rsi_agent_session_protocol::MessageDelivery::NextTurn
                },
                bound_turn_id: None,
                message: AgentMessage {
                    message_id,
                    source: AgentMessageSource::Completion {
                        child_session_id: child_session_id.clone(),
                        activation_id: activation_id.clone(),
                    },
                    content: vec![AgentMessageContent::Text {
                        text: completion_message(outcome),
                    }],
                    options: MessageOptions::default(),
                },
                root_session_id: agent_root_and_path(&parent_header).0,
                target: if parent_has_step {
                    MessageTarget::NextStep
                } else {
                    MessageTarget::NextTurn
                },
                wake_required: !parent_has_step,
            },
        )
        .map_err(|error| TurnError::Invalid(error.to_string()))?;
        Ok(AtomicSessionAppend {
            session_id: parent_session_id.clone(),
            expected_fact_seq,
            expected_control_seq: mailbox.durable_control_seq,
            header: None,
            facts: Vec::new(),
            controls: vec![control],
        })
    }

    pub(super) fn install_committed_activation_terminal(
        &self,
        claim: &TurnClaim,
        expected_fact_seq: u64,
        terminal_seq: u64,
    ) -> TurnResult<()> {
        let mut state = lock_state(&self.inner);
        self.validate_issued_claim(claim)?;
        if !state
            .sessions
            .get(claim.session_id())
            .and_then(|session| session.turns.get(claim.turn_id()))
            .and_then(|turn| turn.claim.as_ref())
            .is_some_and(|owner| {
                owner.claim == claim.claim_id() && owner.live_seq == claim.live_seq()
            })
        {
            return Err(TurnError::StaleClaim);
        }
        let (next, evict_session) = {
            let session = state
                .sessions
                .get_mut(claim.session_id())
                .expect("validated claim session exists");
            if session.durable_seq != expected_fact_seq || !session.pending.is_empty() {
                return Err(TurnError::Invariant(
                    "resident session changed across activation terminal commit".into(),
                ));
            }
            session.durable_seq = terminal_seq;
            session.flush_status.send_replace(FlushStatus {
                durable_seq: terminal_seq,
                permanent_error: session.permanent_flush_error.clone(),
            });
            session.turns.remove(claim.turn_id());
            session
                .turn_order
                .retain(|turn_id| turn_id != claim.turn_id());
            publish_live_watermarks(session);
            (
                session.oldest_claimable().cloned(),
                session.admission_reservations == 0
                    && session.turns.is_empty()
                    && session.pending.is_empty()
                    && !session.header_pending
                    && !session.flush_inflight,
            )
        };
        state
            .queued
            .remove(&(claim.session_id().clone(), claim.turn_id().clone()));
        if let Some(turn_id) = next {
            enqueue(&mut state, claim.session_id().clone(), turn_id);
        }
        if evict_session {
            state.sessions.remove(claim.session_id());
        }
        Ok(())
    }

    #[allow(clippy::too_many_lines)] // Each bounded ancestor iteration repeats one complete compare-and-set settlement protocol.
    pub(super) async fn settle_waiting_ancestors(
        &self,
        mut session_id: SessionId,
    ) -> TurnResult<()> {
        for _ in 0..=rsi_agent_session_protocol::MAXIMUM_AGENT_TREE_DEPTH {
            {
                let Some(active) = self
                    .inner
                    .store
                    .active_activation(&session_id)
                    .await
                    .map_err(turn_store_error)?
                else {
                    return Ok(());
                };
                if active.phase != StoreActivationPhase::WaitingForDescendants {
                    return Ok(());
                }
                let subtree = self
                    .inner
                    .store
                    .read_agent_subtree_snapshot(&session_id)
                    .await
                    .map_err(turn_store_error)?;
                subtree.validate().map_err(turn_store_error)?;
                if subtree.descendants.iter().any(|child| {
                    child.status.has_open_turn
                        || child.status.has_active_activation
                        || child.status.has_waking_message
                }) {
                    return Ok(());
                }
                let turn_id = active.turn_id.clone().ok_or_else(|| {
                    TurnError::Invariant("waiting activation has no indexed Turn".into())
                })?;
                let boundary = self
                    .inner
                    .store
                    .read_turn_boundary(&session_id, &turn_id)
                    .await
                    .map_err(turn_store_error)?;
                let terminal = boundary.terminal().ok_or_else(|| {
                    TurnError::Invariant("waiting activation Turn is not terminal".into())
                })?;
                let outcome = match terminal.body() {
                    SessionFactBody::TurnTerminal { outcome, .. } => outcome.clone(),
                    _ => unreachable!("Store boundary validates terminal Fact kind"),
                };
                let header = read_validated_header_bounded(&self.inner, &session_id)
                    .await
                    .map_err(turn_store_error)?;
                let parent_session_id = header
                    .fork_origin()
                    .map(|origin| origin.parent_session_id.clone());
                let _admissions = self
                    .inner
                    .submission_admission
                    .acquire_many(
                        std::iter::once(session_id.clone())
                            .chain(parent_session_id.iter().cloned()),
                    )
                    .await?;
                let current = self
                    .inner
                    .store
                    .active_activation(&session_id)
                    .await
                    .map_err(turn_store_error)?;
                if current.as_ref() != Some(&active) {
                    return Ok(());
                }
                let mailbox = self
                    .inner
                    .store
                    .read_agent_mailbox_summary(&session_id)
                    .await
                    .map_err(turn_store_error)?;
                if mailbox.durable_fact_seq != boundary.durable_seq() {
                    return Ok(());
                }
                let expected_fact_seq = mailbox.durable_fact_seq;
                let expected_control_seq = mailbox.durable_control_seq;
                let timestamp_ms = self.inner.clock.now_ms().max(1);
                let controls = activation_terminal_controls(
                    expected_control_seq,
                    timestamp_ms,
                    AgentControlRecordBody::ActivationSettled {
                        activation_id: active.activation_id.clone(),
                        outcome: activation_outcome(&outcome),
                    },
                    &mailbox.pending_promotable_message_ids,
                )?;
                let mut sessions = vec![AtomicSessionAppend {
                    session_id: session_id.clone(),
                    expected_fact_seq,
                    expected_control_seq,
                    header: None,
                    facts: Vec::new(),
                    controls,
                }];
                if let Some(parent_session_id) = &parent_session_id {
                    sessions.push(
                        self.completion_append(
                            &session_id,
                            &active.activation_id,
                            parent_session_id,
                            &outcome,
                            timestamp_ms,
                        )
                        .await?,
                    );
                }

                let result = self
                    .commit_agent_with_flush_conflict_retry(AtomicAgentCommit {
                        sessions,
                        required_active_activations: vec![AgentActivationGuard {
                            session_id: session_id.clone(),
                            activation_id: active.activation_id,
                        }],
                        quiescent_descendants_of: Some(session_id.clone()),
                    })
                    .await?;
                match result {
                    Ok(_) => {
                        self.request_ready_scan();
                        let Some(parent_session_id) = parent_session_id else {
                            return Ok(());
                        };
                        session_id = parent_session_id;
                    }
                    Err(
                        StoreError::SessionNotQuiescent { .. }
                        | StoreError::ActivationGuardConflict { .. }
                        | StoreError::Conflict { .. }
                        | StoreError::ControlConflict { .. },
                    ) => return Ok(()),
                    Err(error) => return Err(turn_store_error(error)),
                }
            }
        }
        Err(TurnError::Invariant(
            "waiting-activation settlement exceeded the Agent tree depth".into(),
        ))
    }
}

impl AgentKernel {
    pub(super) async fn settlement_loop(self) {
        let mut cursor = None;
        let mut scan_failed = false;
        let mut next = Instant::now() + WAITING_SETTLEMENT_FALLBACK_INTERVAL;
        let mut backoff = Duration::from_millis(100);
        loop {
            tokio::select! {
                biased;
                () = self.inner.stop_settlement.cancelled() => break,
                () = tokio::time::sleep_until(next) => {},
                () = self.inner.settlement_requested.notified() => {},
            }
            let page = tokio::select! {
                biased;
                () = self.inner.stop_settlement.cancelled() => break,
                page = self.inner.store.list_waiting_activations(cursor.as_ref(), 16) => page.and_then(|page| { page.validate()?; Ok(page) }),
            };
            let page = match page {
                Ok(page) => {
                    backoff = Duration::from_millis(100);
                    page
                }
                Err(error) => {
                    scan_failed = true;
                    self.record_settlement_error(None, &error.to_string());
                    next = Instant::now() + backoff;
                    backoff = (backoff * 2).min(WAITING_SETTLEMENT_FALLBACK_INTERVAL);
                    // Notifications cannot defeat an enumerator's backoff.
                    tokio::select! {
                        () = self.inner.stop_settlement.cancelled() => break,
                        () = tokio::time::sleep_until(next) => {},
                    }
                    continue;
                }
            };
            for session_id in &page.sessions {
                if self.inner.stop_settlement.is_cancelled() {
                    break;
                }
                let kernel = self.clone();
                let target = session_id.clone();
                let result = self
                    .owned_commit(async move { kernel.settle_waiting_ancestors(target).await })
                    .await;
                match result {
                    Ok(()) => self
                        .inner
                        .settlement_health
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .recent_errors
                        .retain(|error| &error.session_id != session_id),
                    Err(TurnError::ShuttingDown) => break,
                    Err(TurnError::Invariant(error)) => {
                        scan_failed = true;
                        self.record_settlement_error(None, &error);
                    }
                    Err(error) => {
                        scan_failed = true;
                        self.record_settlement_error(Some(session_id.clone()), &error.to_string());
                    }
                }
                cursor = Some(session_id.clone());
            }
            if page.has_more {
                next = Instant::now();
                tokio::task::yield_now().await;
            } else {
                if !scan_failed {
                    self.inner
                        .settlement_health
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .global_error = None;
                }
                scan_failed = false;
                cursor = None;
                next = Instant::now() + WAITING_SETTLEMENT_FALLBACK_INTERVAL;
            }
        }
    }

    fn record_settlement_error(&self, session_id: Option<SessionId>, message: &str) {
        let diagnostic = bounded_diagnostic(message);
        let mut health = self
            .inner
            .settlement_health
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        health.failures = health.failures.saturating_add(1);
        if let Some(session_id) = session_id {
            health
                .recent_errors
                .retain(|error| error.session_id != session_id);
            if health.recent_errors.len() == 64 {
                health.recent_errors.remove(0);
            }
            health.recent_errors.push(SettlementSessionError {
                session_id,
                diagnostic,
            });
        } else {
            health.global_error = Some(diagnostic);
        }
    }
}
