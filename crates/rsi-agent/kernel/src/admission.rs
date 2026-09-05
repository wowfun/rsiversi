use super::*;

impl SessionKernel {
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
        self.close_current_step(claim, proposed_outcome).await?;
        let descendants = descendant_session_ids(&self.inner.store, claim.session_id()).await?;
        if !matches!(proposed_outcome, TurnOutcome::Completed) {
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
        staged.budget_usage = enforce_turn_budget(
            claim.header().settings().turn_budget(),
            &budget_original,
            std::slice::from_ref(&terminal),
            terminal.timestamp_ms(),
        )?;
        staged.terminal_seq = Some(terminal.seq());

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
        let quiescent_sessions = descendants;
        let activation_outcome = activation_outcome(&outcome);
        let settled_controls = activation_terminal_controls(
            expected_control_seq,
            terminal.timestamp_ms(),
            AgentControlRecordBody::ActivationSettled {
                activation_id: activation_id.clone(),
                outcome: activation_outcome,
            },
            &mailbox.pending_next_step_completion_message_ids,
        )?;
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
        let settlement = self
            .commit_agent_with_flush_conflict_retry(AtomicAgentCommit {
                sessions,
                required_active_activations: vec![AgentActivationGuard {
                    session_id: claim.session_id().clone(),
                    activation_id: activation_id.clone(),
                }],
                quiescent_sessions,
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
                    &mailbox.pending_next_step_completion_message_ids,
                )?;
                self.inner
                    .store
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
                        quiescent_sessions: Vec::new(),
                    })
                    .await
                    .map_err(turn_store_error)?;
                false
            }
            Err(error) => return Err(turn_store_error(error)),
        };
        self.install_committed_activation_terminal(claim, expected_fact_seq, terminal.seq())?;
        drop(admissions);
        self.inner.claim_changed.notify_waiters();
        if settled
            && let Some(parent_session_id) = parent_session_id
            && let Err(error) = self.settle_waiting_ancestors(parent_session_id).await
        {
            self.inner.settlement_requested.notify_one();
            return Err(error);
        }
        Ok(Some(Arc::new(terminal)))
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
        let parent_header = read_header_bounded(&self.inner, parent_session_id)
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
        self.validate_claim(&state, claim)?;
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
                permanent_error: None,
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
            loop {
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
                let header = read_header_bounded(&self.inner, &session_id)
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
                    continue;
                }
                let mailbox = self
                    .inner
                    .store
                    .read_agent_mailbox_summary(&session_id)
                    .await
                    .map_err(turn_store_error)?;
                if mailbox.durable_fact_seq != boundary.durable_seq() {
                    continue;
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
                    &mailbox.pending_next_step_completion_message_ids,
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
                let descendants = descendant_session_ids(&self.inner.store, &session_id).await?;
                let result = self
                    .commit_agent_with_flush_conflict_retry(AtomicAgentCommit {
                        sessions,
                        required_active_activations: vec![AgentActivationGuard {
                            session_id: session_id.clone(),
                            activation_id: active.activation_id,
                        }],
                        quiescent_sessions: descendants,
                    })
                    .await?;
                match result {
                    Ok(_) => {
                        self.inner.claim_changed.notify_waiters();
                        let Some(parent_session_id) = parent_session_id else {
                            return Ok(());
                        };
                        session_id = parent_session_id;
                        break;
                    }
                    Err(StoreError::SessionNotQuiescent { .. }) => return Ok(()),
                    Err(error) => return Err(turn_store_error(error)),
                }
            }
        }
        Err(TurnError::Invariant(
            "waiting-activation settlement exceeded the Agent tree depth".into(),
        ))
    }
}
