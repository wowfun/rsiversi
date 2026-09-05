use super::*;

#[async_trait]
impl TurnFinalization for SessionKernel {
    fn register(
        &self,
        name: String,
        finalizer: Arc<dyn TurnFinalizer>,
    ) -> rsi_agent_turn_protocol::FinalizationResult<TurnFinalizerLease> {
        validate_identifier("turn finalizer", &name)
            .map_err(|error| TurnFinalizationError::Invalid(error.to_string()))?;
        let registration = {
            let mut state = lock_state(&self.inner);
            if state.finalizer_names.contains(&name) {
                return Err(TurnFinalizationError::Invalid(format!(
                    "turn finalizer `{name}` is already registered"
                )));
            }
            if state.finalizers.len() >= 64 {
                return Err(TurnFinalizationError::Invalid(
                    "turn finalizer capacity is exhausted".into(),
                ));
            }
            state.next_finalizer_registration = state
                .next_finalizer_registration
                .checked_add(1)
                .ok_or_else(|| {
                    TurnFinalizationError::Invalid("turn finalizer identity is exhausted".into())
                })?;
            let registration = state.next_finalizer_registration;
            state.finalizer_names.insert(name.clone());
            state.finalizers.insert(
                registration,
                FinalizerEntry {
                    name: name.clone(),
                    finalizer,
                },
            );
            registration
        };
        let inner = Arc::downgrade(&self.inner);
        Ok(TurnFinalizerLease::new(move || {
            if let Some(inner) = inner.upgrade() {
                let mut state = inner
                    .state
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                if state
                    .finalizers
                    .get(&registration)
                    .is_some_and(|entry| entry.name == name)
                {
                    state.finalizers.remove(&registration);
                    state.finalizer_names.remove(&name);
                }
            }
        }))
    }

    async fn finalize(
        &self,
        context: &TurnFinalizationContext,
    ) -> rsi_agent_turn_protocol::FinalizationResult<TurnFinalizationReport> {
        let finalizers = lock_state(&self.inner)
            .finalizers
            .values()
            .map(|entry| (entry.name.clone(), Arc::clone(&entry.finalizer)))
            .collect::<Vec<_>>();
        let results = futures_util::future::join_all(finalizers.into_iter().map(
            |(name, finalizer)| async move {
                let result = std::panic::AssertUnwindSafe(finalizer.finalize(context))
                    .catch_unwind()
                    .await;
                (name, result)
            },
        ))
        .await;

        for (name, result) in &results {
            match result {
                Ok(Err(error)) => return Err(error.clone()),
                Err(_) => {
                    return Err(TurnFinalizationError::Failed {
                        code: "turn.finalizer_panic".into(),
                        message: format!("turn finalizer `{name}` panicked"),
                    });
                }
                Ok(Ok(_)) => {}
            }
        }
        for (_, result) in results {
            if let Ok(Ok(report)) = result
                && let Some(blocker) = report.completion_blocker()
            {
                return Ok(TurnFinalizationReport::blocked(blocker.clone()));
            }
        }
        Ok(TurnFinalizationReport::complete())
    }
}

#[async_trait]
impl TurnExecution for SessionKernel {
    fn register(&self, executor_id: String) -> TurnResult<ExecutorLease> {
        validate_identifier("executor", &executor_id)
            .map_err(|error| TurnError::Invalid(error.to_string()))?;
        let registration_id = {
            let mut state = lock_state(&self.inner);
            if state.executors.contains_key(&executor_id) {
                return Err(TurnError::Invalid(
                    "executor identity is already registered".into(),
                ));
            }
            state.next_executor_registration = state
                .next_executor_registration
                .checked_add(1)
                .ok_or_else(|| TurnError::Invariant("executor identity exhausted".into()))?;
            let registration_id = state.next_executor_registration;
            state.executors.insert(executor_id.clone(), registration_id);
            registration_id
        };
        self.inner.claim_changed.notify_waiters();
        let inner = Arc::downgrade(&self.inner);
        Ok(ExecutorLease::new(move || {
            deregister_executor(&inner, &executor_id, registration_id);
        }))
    }

    async fn claim(
        &self,
        executor_id: &str,
        cancellation: CancellationToken,
    ) -> TurnResult<Option<TurnClaim>> {
        loop {
            // Create the waiter before inspecting the queue so a notification
            // between the empty-queue check and `select!` is still observed.
            let claim_changed = self.inner.claim_changed.notified();
            {
                let mut state = lock_state(&self.inner);
                let registration_id = state
                    .executors
                    .get(executor_id)
                    .copied()
                    .ok_or(TurnError::StaleClaim)?;
                let candidates = state.claim_queue.len();
                for _ in 0..candidates {
                    let Some((session_id, turn_id)) = state.claim_queue.pop_front() else {
                        break;
                    };
                    state.queued.remove(&(session_id.clone(), turn_id.clone()));
                    let claimable = state.sessions.get(&session_id).is_some_and(|session| {
                        session.permanent_flush_error.is_none()
                            && session.oldest_claimable() == Some(&turn_id)
                            && session
                                .turns
                                .get(&turn_id)
                                .is_some_and(|turn| turn.claim.is_none())
                    });
                    if !claimable {
                        continue;
                    }
                    let root = agent_root_and_path(&state.sessions[&session_id].header).0;
                    state.tree_lanes.retain(|_, pool| pool.strong_count() != 0);
                    let pool = state
                        .tree_lanes
                        .get(&root)
                        .and_then(Weak::upgrade)
                        .unwrap_or_else(|| {
                            let pool = Arc::new(Semaphore::new(MAXIMUM_RUNNING_AGENT_TREE_NODES));
                            state.tree_lanes.insert(root, Arc::downgrade(&pool));
                            pool
                        });
                    let Ok(permit) = Arc::clone(&pool).try_acquire_owned() else {
                        enqueue(&mut state, session_id, turn_id);
                        continue;
                    };
                    state.next_claim = state
                        .next_claim
                        .checked_add(1)
                        .ok_or_else(|| TurnError::Invariant("claim identity exhausted".into()))?;
                    let claim_id = state.next_claim;
                    let session = state
                        .sessions
                        .get_mut(&session_id)
                        .expect("claimable session was observed");
                    let live_seq = session.live_seq().map_err(turn_kernel_error)?;
                    let turn = session
                        .turns
                        .get_mut(&turn_id)
                        .expect("claimable turn was observed");
                    let accepted_at_ms = turn.accepted_at_ms;
                    let accepted_seq = turn.accepted_seq;
                    turn.claim = Some(ClaimOwner {
                        executor: executor_id.into(),
                        registration: registration_id,
                        claim: claim_id,
                        live_seq,
                        tree_lane: Arc::new(TreeClaimLane {
                            pool,
                            permit: Mutex::new(Some(permit)),
                        }),
                    });
                    return Ok(Some(self.inner.claim_issuer.issue(
                        executor_id.into(),
                        claim_id,
                        session_id,
                        turn_id,
                        session.header.clone(),
                        accepted_at_ms,
                        accepted_seq,
                        live_seq,
                    )));
                }
                if !state.accepting {
                    return Ok(None);
                }
            }
            if self.activate_one_ready_message().await? {
                continue;
            }
            tokio::select! {
                () = claim_changed => {}
                () = tokio::time::sleep(READY_SCHEDULER_FALLBACK_INTERVAL) => {}
                () = cancellation.cancelled() => return Ok(None),
                () = self.inner.stop_worker.cancelled() => return Ok(None),
            }
        }
    }

    fn composition(&self, claim: &TurnClaim) -> TurnResult<AgentCompositionPin> {
        let state = lock_state(&self.inner);
        self.validate_claim(&state, claim)?;
        state
            .sessions
            .get(claim.session_id())
            .map(|session| session.composition.clone())
            .ok_or(TurnError::StaleClaim)
    }

    fn agent_caller(&self, claim: &TurnClaim) -> TurnResult<AgentCallerAuthority> {
        let state = lock_state(&self.inner);
        self.validate_claim(&state, claim)?;
        self.inner.claim_issuer.agent_caller(claim)
    }

    async fn read_fork_facts(
        &self,
        claim: &TurnClaim,
        after_parent_seq: u64,
        limit: usize,
    ) -> TurnResult<Option<ForkFactPage>> {
        if limit == 0 || limit > MAXIMUM_FACTS_PER_READ {
            return Err(TurnError::Invalid(
                "fork Fact read limit is out of bounds".into(),
            ));
        }
        {
            let state = lock_state(&self.inner);
            self.validate_claim(&state, claim)?;
        }
        read_fork_page_from_header(&self.inner, claim.header(), after_parent_seq, limit).await
    }

    #[allow(clippy::too_many_lines)] // Safe-boundary entry atomically binds ordered messages, context, and one new Step.
    async fn enter_pending_step_messages(&self, claim: &TurnClaim) -> TurnResult<usize> {
        {
            let state = lock_state(&self.inner);
            if self.validate_claim(&state, claim)?.activation_id.is_none() {
                return Ok(0);
            }
        }
        let _admission = self
            .inner
            .submission_admission
            .acquire(claim.session_id())
            .await?;
        let scan = scan_durable_messages(&self.inner, claim.session_id(), None).await?;
        let pending = scan
            .pending
            .into_iter()
            .filter(|entry| entry.target == MessageTarget::NextStep)
            .collect::<Vec<_>>();
        let pending =
            bounded_step_message_prefix(pending, MAXIMUM_NEXT_STEP_MESSAGE_PAYLOAD_BYTES)?;
        if pending.is_empty() {
            return Ok(0);
        }
        let live_seq = {
            let state = lock_state(&self.inner);
            self.validate_claim(&state, claim)?;
            state
                .sessions
                .get(claim.session_id())
                .ok_or(TurnError::StaleClaim)?
                .live_seq()
                .map_err(turn_kernel_error)?
        };
        self.flush(claim, live_seq).await?;
        let (expected_fact_seq, activation_id, current_step, original, header, current_context) = {
            let state = lock_state(&self.inner);
            let turn = self.validate_claim(&state, claim)?;
            let session = state
                .sessions
                .get(claim.session_id())
                .expect("validated claim session exists");
            (
                session.durable_seq,
                turn.activation_id.clone().ok_or_else(|| {
                    TurnError::Invalid(
                        "next-Step Agent message requires an activation-owned Turn".into(),
                    )
                })?,
                turn.current_step.clone().ok_or_else(|| {
                    TurnError::Invariant(
                        "activation-owned Turn has no open Step at a safe boundary".into(),
                    )
                })?,
                clone_turn_control(turn),
                session.header.clone(),
                session.workspace_context.clone(),
            )
        };
        let next_step = rsi_agent_session_protocol::StepId::new(format!(
            "step-message-{}",
            pending[0].accepted_control_seq
        ))
        .map_err(|error| TurnError::Invalid(error.to_string()))?;
        let mut bodies = vec![
            SessionFactBody::StepEnded {
                turn_id: claim.turn_id().clone(),
                step_id: current_step,
                outcome: StepOutcome::Completed,
            },
            SessionFactBody::StepStarted {
                turn_id: claim.turn_id().clone(),
                step_id: next_step.clone(),
            },
        ];
        let context_snapshot = self
            .inner
            .workspace_context
            .snapshot(
                &header,
                &pending
                    .iter()
                    .map(|entry| &entry.message)
                    .collect::<Vec<_>>(),
            )
            .await
            .map_err(|error| TurnError::Invalid(error.to_string()))?;
        let (background, invocations, next_context) = workspace_context_bodies(
            claim.turn_id(),
            &next_step,
            &current_context,
            context_snapshot,
        );
        let background_len = background.len();
        bodies.extend(background);
        bodies.extend(
            pending
                .iter()
                .map(|entry| SessionFactBody::InputMessageEntered {
                    turn_id: claim.turn_id().clone(),
                    step_id: next_step.clone(),
                    source: entered_message_source(&entry.message),
                    content: entry.message.content.clone(),
                }),
        );
        bodies.extend(invocations);
        let timestamp_ms = self.inner.clock.now_ms().max(1);
        let facts = bodies
            .into_iter()
            .enumerate()
            .map(|(offset, body)| {
                SessionFact::new(
                    expected_fact_seq
                        .checked_add(
                            u64::try_from(offset)
                                .map_err(|_| {
                                    TurnError::Invariant("Fact offset exceeds u64".into())
                                })?
                                .checked_add(1)
                                .ok_or_else(|| {
                                    TurnError::Invariant("Fact offset exhausted".into())
                                })?,
                        )
                        .ok_or_else(|| TurnError::Invariant("Fact sequence exhausted".into()))?,
                    timestamp_ms,
                    body,
                )
                .map_err(|error| TurnError::Invalid(error.to_string()))
            })
            .collect::<TurnResult<Vec<_>>>()?;
        let budget_usage = enforce_turn_budget(
            claim.header().settings().turn_budget(),
            &original,
            &facts,
            timestamp_ms,
        )?;
        let mut staged = original;
        for fact in &facts {
            apply_executor_body(&mut staged, fact.body())?;
        }
        staged.budget_usage = budget_usage;
        let controls = pending
            .iter()
            .enumerate()
            .map(|(offset, entry)| {
                let fact_index = offset
                    .checked_add(2)
                    .and_then(|index| index.checked_add(background_len))
                    .ok_or_else(|| TurnError::Invariant("Fact offset exhausted".into()))?;
                let control_offset = u64::try_from(offset)
                    .map_err(|_| TurnError::Invariant("control offset exceeds u64".into()))?;
                let control_offset = control_offset
                    .checked_add(1)
                    .ok_or_else(|| TurnError::Invariant("control offset exhausted".into()))?;
                AgentControlRecord::new(
                    scan.durable_control_seq
                        .checked_add(control_offset)
                        .ok_or_else(|| TurnError::Invariant("control sequence exhausted".into()))?,
                    timestamp_ms,
                    AgentControlRecordBody::MessageClaimed {
                        message_id: entry.message.message_id.clone(),
                        activation_id: activation_id.clone(),
                        turn_id: claim.turn_id().clone(),
                        step_id: next_step.clone(),
                        entered_fact_seq: facts
                            .get(fact_index)
                            .ok_or_else(|| {
                                TurnError::Invariant(
                                    "Step message Fact/control cardinality diverged".into(),
                                )
                            })?
                            .seq(),
                    },
                )
                .map_err(|error| TurnError::Invalid(error.to_string()))
            })
            .collect::<TurnResult<Vec<_>>>()?;
        self.inner
            .store
            .commit_agent(AtomicAgentCommit {
                sessions: vec![AtomicSessionAppend {
                    session_id: claim.session_id().clone(),
                    expected_fact_seq,
                    expected_control_seq: scan.durable_control_seq,
                    header: None,
                    facts: facts.clone(),
                    controls,
                }],
                required_active_activations: Vec::new(),
                quiescent_sessions: Vec::new(),
            })
            .await
            .map_err(turn_store_error)?;
        {
            let mut state = lock_state(&self.inner);
            self.validate_claim(&state, claim)?;
            let session = state
                .sessions
                .get_mut(claim.session_id())
                .expect("validated claim session exists");
            if session.durable_seq != expected_fact_seq || !session.pending.is_empty() {
                return Err(TurnError::Invariant(
                    "resident session changed across next-Step message commit".into(),
                ));
            }
            let turn = session
                .turns
                .get_mut(claim.turn_id())
                .expect("validated claim turn exists");
            *turn = staged;
            session.workspace_context = next_context;
            session.durable_seq = facts
                .last()
                .expect("Step message commit contains Facts")
                .seq();
            session.flush_status.send_replace(FlushStatus {
                durable_seq: session.durable_seq,
                permanent_error: None,
            });
            publish_live_watermarks(session);
        }
        Ok(pending.len())
    }

    async fn refresh_workspace_context(&self, claim: &TurnClaim) -> TurnResult<usize> {
        let (header, step_id, current_context) = {
            let state = lock_state(&self.inner);
            let turn = self.validate_claim(&state, claim)?;
            let session = state
                .sessions
                .get(claim.session_id())
                .expect("validated claim session exists");
            (
                session.header.clone(),
                turn.current_step.clone(),
                session.workspace_context.clone(),
            )
        };
        let snapshot = self
            .inner
            .workspace_context
            .snapshot(&header, &[])
            .await
            .map_err(|error| TurnError::Invalid(error.to_string()))?;
        if !snapshot.complete {
            return Ok(0);
        }
        let step_id = step_id.ok_or_else(|| {
            TurnError::Invariant("workspace context refresh requires one open Agent Step".into())
        })?;
        let (mut bodies, invocations, _) =
            workspace_context_bodies(claim.turn_id(), &step_id, &current_context, snapshot);
        if !invocations.is_empty() {
            return Err(TurnError::Invariant(
                "workspace refresh invented a direct-user skill invocation".into(),
            ));
        }
        if bodies.is_empty() {
            return Ok(0);
        }
        let body_count = bodies.len();
        loop {
            match self.publish(claim, bodies).await? {
                PublishAttempt::Published(facts) => {
                    let through_seq = facts
                        .last()
                        .expect("nonempty workspace context publication")
                        .seq();
                    self.flush(claim, through_seq).await?;
                    return Ok(body_count);
                }
                PublishAttempt::FlushRequired { unpublished } => {
                    let live_seq = lock_state(&self.inner)
                        .sessions
                        .get(claim.session_id())
                        .ok_or(TurnError::StaleClaim)?
                        .live_seq()
                        .map_err(turn_kernel_error)?;
                    self.flush(claim, live_seq).await?;
                    bodies = unpublished;
                }
            }
        }
    }

    async fn close_current_step(&self, claim: &TurnClaim, outcome: &TurnOutcome) -> TurnResult<()> {
        let step_id = {
            let state = lock_state(&self.inner);
            self.validate_claim(&state, claim)?.current_step.clone()
        };
        let Some(step_id) = step_id else {
            return Ok(());
        };
        let step_outcome = if matches!(outcome, TurnOutcome::Completed) {
            StepOutcome::Completed
        } else {
            StepOutcome::Stopped {
                reason: bounded_diagnostic(&format!("Turn ended with {outcome:?}")),
            }
        };
        let mut bodies = vec![SessionFactBody::StepEnded {
            turn_id: claim.turn_id().clone(),
            step_id,
            outcome: step_outcome,
        }];
        loop {
            match self.publish(claim, bodies).await? {
                PublishAttempt::Published(_) => return Ok(()),
                PublishAttempt::FlushRequired { unpublished } => {
                    let live_seq = lock_state(&self.inner)
                        .sessions
                        .get(claim.session_id())
                        .ok_or(TurnError::StaleClaim)?
                        .live_seq()
                        .map_err(turn_kernel_error)?;
                    self.flush(claim, live_seq).await?;
                    bodies = unpublished;
                }
            }
        }
    }

    async fn finish_activation_turn(
        &self,
        claim: &TurnClaim,
        outcome: &TurnOutcome,
    ) -> TurnResult<Option<Arc<SessionFact>>> {
        self.finish_activation_claim(claim, outcome).await
    }

    async fn read_facts(
        &self,
        claim: &TurnClaim,
        after_seq: u64,
        limit: usize,
    ) -> TurnResult<ClaimFactPage> {
        if limit == 0 || limit > MAXIMUM_FACTS_PER_READ {
            return Err(TurnError::Invalid(
                "Fact read limit is out of bounds".into(),
            ));
        }
        let (durable_seq, live_seq, hidden_turns) = {
            let state = lock_state(&self.inner);
            self.validate_claim(&state, claim)?;
            let session = state
                .sessions
                .get(claim.session_id())
                .expect("validated claim session exists");
            let claimed_index = session
                .turn_order
                .iter()
                .position(|turn_id| turn_id == claim.turn_id())
                .ok_or_else(|| TurnError::Invariant("claimed turn is missing from order".into()))?;
            (
                session.durable_seq,
                session.live_seq().map_err(turn_kernel_error)?,
                session.turn_order[claimed_index + 1..]
                    .iter()
                    .cloned()
                    .collect::<BTreeSet<_>>(),
            )
        };
        if after_seq > live_seq {
            return Err(TurnError::Invalid("Fact cursor exceeds live tail".into()));
        }
        let mut facts = Vec::new();
        let mut through_seq = after_seq;
        let mut scanned = 0_usize;
        if after_seq < durable_seq {
            let page = read_facts_bounded(&self.inner, claim.session_id(), after_seq, limit)
                .await
                .map_err(turn_store_error)?;
            scanned = page.facts.len();
            through_seq = page
                .facts
                .last()
                .map_or(after_seq, SessionFact::seq)
                .min(live_seq);
            facts.extend(page.facts.into_iter().filter_map(|fact| {
                (fact.seq() <= live_seq && !hidden_turns.contains(fact.body().turn_id()))
                    .then(|| Arc::new(fact))
            }));
        }
        let state = lock_state(&self.inner);
        self.validate_claim(&state, claim)?;
        let session = state
            .sessions
            .get(claim.session_id())
            .expect("validated claim session exists");
        if through_seq < session.durable_seq || through_seq == live_seq || scanned == limit {
            return Ok(ClaimFactPage { facts, through_seq });
        }
        if scanned < limit && through_seq < live_seq {
            let pending_after = through_seq;
            for fact in session
                .pending
                .iter()
                .filter(|fact| fact.seq() > pending_after && fact.seq() <= live_seq)
                .take(limit - scanned)
            {
                through_seq = fact.seq();
                if !hidden_turns.contains(fact.body().turn_id()) {
                    facts.push(fact.clone());
                }
            }
        }
        Ok(ClaimFactPage { facts, through_seq })
    }

    async fn read_checkpoint_facts(
        &self,
        claim: &TurnClaim,
        after_seq: u64,
        limit: usize,
    ) -> TurnResult<Option<ClaimFactPage>> {
        self.validate_issued_claim(claim)?;
        if limit == 0 || limit > MAXIMUM_FACTS_PER_READ {
            return Err(TurnError::Invalid(
                "checkpoint Fact read limit is out of bounds".into(),
            ));
        }
        if !context_checkpoints_enabled(&self.inner) {
            return Ok(None);
        }
        let (session_present, expected_durable) = {
            let state = lock_state(&self.inner);
            match state.sessions.get(claim.session_id()) {
                Some(session) => {
                    let live_seq = session.live_seq().map_err(turn_kernel_error)?;
                    (
                        true,
                        (live_seq == session.durable_seq).then_some(session.durable_seq),
                    )
                }
                None => (false, None),
            }
        };
        if session_present && expected_durable.is_none() {
            return Ok(None);
        }
        if read_stored_outcome(&self.inner, claim.session_id(), claim.turn_id())
            .await?
            .is_none()
        {
            return Ok(None);
        }
        let page = read_facts_bounded(&self.inner, claim.session_id(), after_seq, limit)
            .await
            .map_err(turn_store_error)?;
        if expected_durable.is_some_and(|expected| page.durable_seq != expected) {
            return Ok(None);
        }
        let through_seq = page.facts.last().map_or(after_seq, SessionFact::seq);
        Ok(Some(ClaimFactPage {
            facts: page.facts.into_iter().map(Arc::new).collect(),
            through_seq,
        }))
    }

    async fn read_checkpoint_fork_facts(
        &self,
        claim: &TurnClaim,
        after_parent_seq: u64,
        limit: usize,
    ) -> TurnResult<Option<ForkFactPage>> {
        self.validate_issued_claim(claim)?;
        if limit == 0 || limit > MAXIMUM_FACTS_PER_READ {
            return Err(TurnError::Invalid(
                "checkpoint fork Fact read limit is out of bounds".into(),
            ));
        }
        if !context_checkpoints_enabled(&self.inner) {
            return Ok(None);
        }
        let resident_is_stable = {
            let state = lock_state(&self.inner);
            state
                .sessions
                .get(claim.session_id())
                .is_none_or(|session| {
                    session
                        .live_seq()
                        .is_ok_and(|live| live == session.durable_seq)
                })
        };
        if !resident_is_stable
            || read_stored_outcome(&self.inner, claim.session_id(), claim.turn_id())
                .await?
                .is_none()
        {
            return Ok(None);
        }
        read_fork_page_from_header(&self.inner, claim.header(), after_parent_seq, limit).await
    }

    async fn read_context_checkpoint(
        &self,
        session_id: &SessionId,
    ) -> TurnResult<Option<ContextCheckpoint>> {
        if !context_checkpoints_enabled(&self.inner) {
            return Ok(None);
        }
        let permits = u32::try_from(MAXIMUM_CONTEXT_CHECKPOINT_BYTES)
            .map_err(|_| TurnError::Invariant("checkpoint bound exceeds semaphore range".into()))?;
        let permit = Arc::clone(&self.inner.store_read_admission)
            .acquire_many_owned(permits)
            .await
            .map_err(|_| TurnError::Invariant("Store-read admission closed".into()))?;
        let checkpoint = match self.inner.store.read_context_checkpoint(session_id).await {
            Ok(checkpoint) => checkpoint,
            Err(StoreError::NotFound(_)) => None,
            Err(error) => return Err(turn_store_error(error)),
        };
        drop(permit);
        Ok(checkpoint.map(|checkpoint| ContextCheckpoint {
            header_fingerprint: checkpoint.header_fingerprint,
            through_seq: checkpoint.through_seq,
            fact_prefix_sha256: checkpoint.fact_prefix_sha256,
            bytes: checkpoint.bytes,
        }))
    }

    async fn write_context_checkpoint(
        &self,
        claim: &TurnClaim,
        checkpoint: ContextCheckpoint,
    ) -> TurnResult<bool> {
        self.validate_issued_claim(claim)?;
        let expected_fingerprint = claim
            .header()
            .fingerprint()
            .map_err(|error| TurnError::Invalid(error.to_string()))?;
        if checkpoint.header_fingerprint != expected_fingerprint {
            return Err(TurnError::Invalid(
                "checkpoint header fingerprint changed session identity".into(),
            ));
        }
        if !context_checkpoints_enabled(&self.inner) {
            return Ok(false);
        }
        {
            let state = lock_state(&self.inner);
            if let Some(session) = state.sessions.get(claim.session_id())
                && (!self
                    .inner
                    .claim_issuer
                    .validates_header(claim, &session.header)
                    || session.durable_seq != checkpoint.through_seq
                    || session.live_seq().map_err(turn_kernel_error)? != checkpoint.through_seq)
            {
                return Ok(false);
            }
        }
        if read_stored_outcome(&self.inner, claim.session_id(), claim.turn_id())
            .await?
            .is_none()
        {
            return Ok(false);
        }
        let write = WriteContextCheckpoint {
            session_id: claim.session_id().clone(),
            expected_durable_seq: checkpoint.through_seq,
            checkpoint: StoredContextCheckpoint {
                header_fingerprint: checkpoint.header_fingerprint,
                through_seq: checkpoint.through_seq,
                fact_prefix_sha256: checkpoint.fact_prefix_sha256,
                bytes: checkpoint.bytes,
            },
        };
        match self.inner.store.write_context_checkpoint(write).await {
            Ok(()) => Ok(true),
            Err(StoreError::Conflict { .. }) => Ok(false),
            Err(error) => Err(turn_store_error(error)),
        }
    }

    async fn publish(
        &self,
        claim: &TurnClaim,
        mut bodies: Vec<SessionFactBody>,
    ) -> TurnResult<PublishAttempt> {
        if bodies.is_empty() || bodies.len() > MAXIMUM_STORE_BATCH_FACTS {
            return Err(TurnError::Invalid(
                "Fact publication batch is empty or too large".into(),
            ));
        }
        let deadline = Instant::now() + DURABILITY_WAIT_TIMEOUT;
        loop {
            let process_capacity_changed = self.inner.process_pending_changed.notified();
            tokio::pin!(process_capacity_changed);
            let _enabled = process_capacity_changed.as_mut().enable();
            let admission = self
                .inner
                .submission_admission
                .acquire(claim.session_id())
                .await?;
            let attempted = try_publish_once(self, claim, bodies)?;
            drop(admission);
            match attempted {
                PublishAdmission::Complete(result) => return Ok(result),
                PublishAdmission::ProcessPressure(unpublished) => {
                    bodies = unpublished;
                    self.inner.flush_requested.notify_one();
                    tokio::select! {
                        () = &mut process_capacity_changed => {}
                        () = self.inner.stop_worker.cancelled() => {
                            return Err(TurnError::ShuttingDown);
                        }
                        () = tokio::time::sleep_until(deadline) => {
                            return Err(TurnError::Capacity);
                        }
                    }
                }
            }
        }
    }

    async fn flush(&self, claim: &TurnClaim, through_seq: u64) -> TurnResult<u64> {
        let status = {
            let state = lock_state(&self.inner);
            self.validate_claim(&state, claim)?;
            let session = state
                .sessions
                .get(claim.session_id())
                .expect("validated claim session exists");
            let live_seq = session.live_seq().map_err(turn_kernel_error)?;
            if through_seq == 0 || through_seq > live_seq {
                return Err(TurnError::Invalid(
                    "flush target is zero or exceeds the live tail".into(),
                ));
            }
            session.flush_status.subscribe()
        };
        self.inner.flush_requested.notify_one();
        self.wait_on_flush_status(status, through_seq)
            .await
            .map_err(turn_kernel_error)
    }

    fn cancellation(&self, claim: &TurnClaim) -> TurnResult<CancellationToken> {
        let state = lock_state(&self.inner);
        let turn = self.validate_claim(&state, claim)?;
        Ok(turn.cancellation.clone())
    }

    fn release(&self, claim: &TurnClaim) -> TurnResult<()> {
        let mut state = lock_state(&self.inner);
        self.validate_claim(&state, claim)?;
        let turn = state
            .sessions
            .get_mut(claim.session_id())
            .expect("validated claim session exists")
            .turns
            .get_mut(claim.turn_id())
            .expect("validated claim turn exists");
        if turn.terminal.is_none() {
            turn.claim = None;
            enqueue(
                &mut state,
                claim.session_id().clone(),
                claim.turn_id().clone(),
            );
        }
        drop(state);
        self.inner.claim_changed.notify_waiters();
        Ok(())
    }
}

pub(super) enum PublishAdmission {
    Complete(PublishAttempt),
    ProcessPressure(Vec<SessionFactBody>),
}

#[allow(clippy::too_many_lines)] // Staging keeps budget, intent fences, and speculative suffix mutation all-or-nothing.
pub(super) fn try_publish_once(
    kernel: &SessionKernel,
    claim: &TurnClaim,
    bodies: Vec<SessionFactBody>,
) -> TurnResult<PublishAdmission> {
    let mut state = lock_state(&kernel.inner);
    kernel.validate_claim(&state, claim)?;
    if !state.accepting {
        return Err(TurnError::ShuttingDown);
    }
    let session = state
        .sessions
        .get_mut(claim.session_id())
        .expect("validated claim session exists");
    if let Some(error) = &session.permanent_flush_error {
        return Err(TurnError::Flush(error.clone()));
    }
    let original = session
        .turns
        .get(claim.turn_id())
        .expect("validated claim turn exists");
    let mut staged = clone_turn_control(original);
    let mut staged_workspace_context = session.workspace_context.clone();
    let mut normalized = Vec::with_capacity(bodies.len());
    for body in bodies {
        if body.turn_id() != claim.turn_id() {
            return Err(TurnError::Invalid(
                "executor Fact changed the claimed turn identity".into(),
            ));
        }
        validate_durable_intent_fence(session, &body)?;
        let body = canonicalize_terminal(body, staged.cancel_requested);
        apply_executor_body(&mut staged, &body)?;
        apply_workspace_context_state(&mut staged_workspace_context, &body);
        normalized.push(body);
    }
    let mut next_seq = session.live_seq().map_err(turn_kernel_error)?;
    let mut facts = Vec::with_capacity(normalized.len());
    let mut added_bytes = 0_usize;
    for body in normalized {
        next_seq = next_seq
            .checked_add(1)
            .ok_or_else(|| TurnError::Invariant("Fact sequence exhausted".into()))?;
        let fact = SessionFact::new(next_seq, kernel.inner.clock.now_ms().max(1), body)
            .map_err(|error| TurnError::Invalid(error.to_string()))?;
        added_bytes = added_bytes
            .checked_add(fact.encoded_len())
            .ok_or_else(|| TurnError::Invalid("Fact bytes overflowed".into()))?;
        facts.push(fact);
    }
    staged.budget_usage = enforce_turn_budget(
        session.header.settings().turn_budget(),
        original,
        &facts,
        kernel.inner.clock.now_ms().max(1),
    )?;
    let projected_pending_bytes = session
        .pending_bytes
        .checked_add(added_bytes)
        .ok_or_else(|| TurnError::Invariant("pending Fact bytes overflowed".into()))?;
    if added_bytes > MAXIMUM_PENDING_FACT_BYTES
        || added_bytes > kernel.inner.limits.maximum_process_pending_fact_bytes
    {
        return Err(TurnError::Invalid(
            "Fact publication batch exceeds an empty pending-byte budget".into(),
        ));
    }
    if projected_pending_bytes > MAXIMUM_PENDING_FACT_BYTES
        || projected_pending_bytes > kernel.inner.limits.maximum_process_pending_fact_bytes
    {
        return Ok(PublishAdmission::Complete(PublishAttempt::FlushRequired {
            unpublished: facts.into_iter().map(SessionFact::into_body).collect(),
        }));
    }
    match reserve_atomic_capacity(
        &kernel.inner.process_pending_bytes,
        added_bytes,
        kernel.inner.limits.maximum_process_pending_fact_bytes,
    ) {
        Ok(()) => {}
        Err(KernelError::Capacity(_)) => {
            return Ok(PublishAdmission::ProcessPressure(
                facts.into_iter().map(SessionFact::into_body).collect(),
            ));
        }
        Err(error) => return Err(turn_kernel_error(error)),
    }
    let facts = facts.into_iter().map(Arc::new).collect::<Vec<_>>();
    if staged.terminal.is_some() {
        staged.terminal_seq = facts.last().map(|fact| fact.seq());
    }
    *session
        .turns
        .get_mut(claim.turn_id())
        .expect("validated claim turn exists") = staged;
    session.workspace_context = staged_workspace_context;
    for fact in &facts {
        session.pending_bytes = session
            .pending_bytes
            .checked_add(fact.encoded_len())
            .expect("the complete batch pending-byte projection was validated");
        session.pending.push_back(fact.clone());
        if !is_terminal_fact(fact) {
            publish_live_watermarks(session);
        }
    }
    Ok(PublishAdmission::Complete(PublishAttempt::Published(facts)))
}
