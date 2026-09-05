use super::*;

impl SessionKernel {
    /// Recovers every durable session and repairs unfinished tails before return.
    pub async fn recover(
        store: Arc<dyn SessionStore>,
        composition: Arc<dyn AgentComposition>,
    ) -> Result<Self> {
        Self::recover_with_clock(store, composition, Arc::new(SystemClock)).await
    }

    /// Recovers with a deterministic timestamp source.
    pub async fn recover_with_clock(
        store: Arc<dyn SessionStore>,
        composition: Arc<dyn AgentComposition>,
        clock: Arc<dyn Clock>,
    ) -> Result<Self> {
        Self::recover_with_clock_and_limits(store, composition, clock, KernelLimits::default())
            .await
    }

    /// Recovers with a deterministic clock and explicit process-wide limits.
    pub async fn recover_with_clock_and_limits(
        store: Arc<dyn SessionStore>,
        composition: Arc<dyn AgentComposition>,
        clock: Arc<dyn Clock>,
        limits: KernelLimits,
    ) -> Result<Self> {
        Self::recover_with_context_clock_and_limits(
            store,
            composition,
            Arc::new(EmptyWorkspaceContext),
            clock,
            limits,
        )
        .await
    }

    /// Recovers with an explicit process-local workspace context source, clock, and limits.
    pub async fn recover_with_context_clock_and_limits(
        store: Arc<dyn SessionStore>,
        composition: Arc<dyn AgentComposition>,
        workspace_context: Arc<dyn WorkspaceContext>,
        clock: Arc<dyn Clock>,
        limits: KernelLimits,
    ) -> Result<Self> {
        limits.validate()?;
        let mut after = None;
        loop {
            let page = store
                .list_open_sessions(after.as_ref(), MAXIMUM_SESSIONS_PER_READ)
                .await?;
            let has_more = page.has_more;
            let next_after = page.sessions.last().cloned();
            for session_id in page.sessions {
                repair_unfinished_session(&store, clock.as_ref(), &session_id).await?;
            }
            if !has_more {
                break;
            }
            after = Some(next_after.ok_or_else(|| {
                KernelError::Invariant("session enumeration made no progress".into())
            })?);
        }
        let kernel = Self {
            inner: Arc::new(KernelInner {
                store,
                composition,
                workspace_context,
                resume_issuer: ResumeAdmissionIssuer::new(),
                claim_issuer: TurnClaimIssuer::new(),
                clock,
                state: Mutex::new(KernelState {
                    accepting: true,
                    sessions: BTreeMap::new(),
                    loading_sessions: BTreeMap::new(),
                    fresh_reservations: BTreeSet::new(),
                    executors: BTreeMap::new(),
                    next_executor_registration: 0,
                    finalizers: BTreeMap::new(),
                    finalizer_names: BTreeSet::new(),
                    next_finalizer_registration: 0,
                    tree_lanes: BTreeMap::new(),
                    next_claim: 0,
                    claim_queue: VecDeque::new(),
                    queued: BTreeSet::new(),
                }),
                submission_admission: SubmissionAdmission::new(),
                ready_activation: AsyncMutex::new(None),
                claim_changed: Notify::new(),
                flush_requested: Notify::new(),
                settlement_requested: Notify::new(),
                stop_worker: CancellationToken::new(),
                limits,
                process_pending_bytes: AtomicUsize::new(0),
                process_pending_changed: Notify::new(),
                active_observers: AtomicUsize::new(0),
                store_read_admission: Arc::new(Semaphore::new(limits.maximum_store_read_bytes)),
            }),
        };
        kernel.reconcile_waiting_activations().await?;
        Ok(kernel)
    }

    /// Starts the sole background write-behind worker.
    pub fn start_write_behind(&self) -> JoinHandle<()> {
        let kernel = self.clone();
        let first_tick = Instant::now() + WRITE_BEHIND_INTERVAL;
        let first_settlement = Instant::now() + WAITING_SETTLEMENT_FALLBACK_INTERVAL;
        tokio::spawn(async move { kernel.flush_loop(first_tick, first_settlement).await })
    }

    /// Stops admission, durably drains pending Facts, ends the worker, and
    /// releases resident generation pins.
    pub async fn shutdown(&self, mut worker: JoinHandle<()>) -> Result<()> {
        {
            let mut state = lock_state(&self.inner);
            state.accepting = false;
        }
        self.inner.submission_admission.close();
        self.inner.claim_changed.notify_waiters();
        self.inner.flush_requested.notify_waiters();
        let flush_result =
            match tokio::time::timeout(SHUTDOWN_FLUSH_TIMEOUT, self.flush_every_session()).await {
                Ok(result) => result,
                Err(_) => Err(KernelError::Shutdown("final flush timed out".into())),
            };
        self.inner.stop_worker.cancel();
        self.inner.flush_requested.notify_waiters();
        let worker_result = if let Ok(result) =
            tokio::time::timeout(SHUTDOWN_FLUSH_TIMEOUT, &mut worker).await
        {
            result.map_err(|error| KernelError::Shutdown(format!("flush worker failed: {error}")))
        } else {
            worker.abort();
            let _ = worker.await;
            Err(KernelError::Shutdown(
                "flush worker did not stop before the shutdown deadline".into(),
            ))
        };
        self.quiesce();
        flush_result.and(worker_result)
    }

    pub(super) fn quiesce(&self) {
        let (sessions, loads, finalizers) = {
            let mut state = lock_state(&self.inner);
            let sessions = std::mem::take(&mut state.sessions);
            let loads = std::mem::take(&mut state.loading_sessions)
                .into_values()
                .collect::<Vec<_>>();
            let finalizers = std::mem::take(&mut state.finalizers);
            state.fresh_reservations.clear();
            state.executors.clear();
            state.finalizer_names.clear();
            state.claim_queue.clear();
            state.queued.clear();
            (sessions, loads, finalizers)
        };
        self.inner.process_pending_bytes.store(0, Ordering::Release);
        self.inner.process_pending_changed.notify_waiters();
        for session in sessions.values() {
            for turn in session.turns.values() {
                turn.cancellation.cancel();
            }
        }
        for load in loads {
            load.complete(Err(TurnError::ShuttingDown));
        }
        self.inner.claim_changed.notify_waiters();
        drop(sessions);
        drop(finalizers);
    }

    pub(super) async fn flush_loop(self, mut next_tick: Instant, mut next_settlement: Instant) {
        loop {
            enum WorkerTask {
                Flush,
                Settle,
            }
            let task = tokio::select! {
                () = tokio::time::sleep_until(next_tick) => WorkerTask::Flush,
                () = self.inner.flush_requested.notified() => WorkerTask::Flush,
                () = tokio::time::sleep_until(next_settlement) => WorkerTask::Settle,
                () = self.inner.settlement_requested.notified() => WorkerTask::Settle,
                () = self.inner.stop_worker.cancelled() => break,
            };
            match task {
                WorkerTask::Flush => {
                    self.flush_ready_sessions().await;
                    next_tick = rebase_write_behind_tick(next_tick, Instant::now());
                }
                WorkerTask::Settle => {
                    let _result = self.reconcile_waiting_activations().await;
                    next_settlement = Instant::now() + WAITING_SETTLEMENT_FALLBACK_INTERVAL;
                }
            }
        }
    }

    pub(super) async fn flush_ready_sessions(&self) {
        let session_ids = {
            let state = lock_state(&self.inner);
            state.sessions.keys().cloned().collect::<Vec<_>>()
        };
        for session_id in session_ids {
            let Some(prepared) = self.prepare_flush_batch(&session_id) else {
                continue;
            };
            let batch = prepared.into_store_batch();
            let result = self.inner.store.append(batch).await;
            self.complete_flush(&session_id, result);
        }
    }

    pub(super) fn prepare_flush_batch(&self, session_id: &SessionId) -> Option<PreparedFlushBatch> {
        let mut state = lock_state(&self.inner);
        let session = state.sessions.get_mut(session_id)?;
        if session.flush_inflight
            || session.pending.is_empty()
            || session.permanent_flush_error.is_some()
            || session
                .retry_not_before
                .is_some_and(|deadline| deadline > Instant::now())
        {
            return None;
        }
        let mut facts = Vec::new();
        let mut bytes = 0_usize;
        for fact in &session.pending {
            if facts.len() == MAXIMUM_STORE_BATCH_FACTS {
                break;
            }
            let encoded = fact.encoded_len();
            if !facts.is_empty() && bytes.saturating_add(encoded) > MAXIMUM_STORE_BATCH_BYTES {
                break;
            }
            bytes = bytes.saturating_add(encoded);
            facts.push(Arc::clone(fact));
        }
        session.flush_inflight = true;
        Some(PreparedFlushBatch {
            session_id: session_id.clone(),
            expected_seq: session.durable_seq,
            header: session
                .header_pending
                .then(|| session.header.as_ref().clone()),
            facts,
        })
    }

    pub(super) fn complete_flush(
        &self,
        session_id: &SessionId,
        result: std::result::Result<rsi_agent_store_protocol::AppendCommit, StoreError>,
    ) {
        let mut request_more = false;
        let mut enqueue_after_commit = false;
        let mut claim_available = false;
        let mut pruned_turns = Vec::new();
        let mut evict_session = false;
        let mut released_process_capacity = false;
        let mut latched_permanent_failure = false;
        {
            let mut state = lock_state(&self.inner);
            let Some(session) = state.sessions.get_mut(session_id) else {
                return;
            };
            session.flush_inflight = false;
            match result {
                Ok(commit) => {
                    pruned_turns =
                        apply_committed_flush(session, commit, &self.inner.process_pending_bytes);
                    released_process_capacity = true;
                    enqueue_after_commit = true;
                    request_more = !session.pending.is_empty();
                    evict_session = session.admission_reservations == 0
                        && session.turns.is_empty()
                        && session.pending.is_empty();
                }
                Err(StoreError::Io(_)) => {
                    session.retry_failures = session.retry_failures.saturating_add(1);
                    if session.retry_failures >= MAXIMUM_CONSECUTIVE_FLUSH_FAILURES {
                        session.permanent_flush_error = Some(format!(
                            "Store append failed {MAXIMUM_CONSECUTIVE_FLUSH_FAILURES} consecutive times"
                        ));
                        latched_permanent_failure = true;
                        let _previous = session.flush_status.send_replace(FlushStatus {
                            durable_seq: session.durable_seq,
                            permanent_error: session.permanent_flush_error.clone(),
                        });
                    } else {
                        let shift = session.retry_failures.saturating_sub(1).min(6);
                        let multiplier = 1_u32 << shift;
                        let backoff = MINIMUM_RETRY_BACKOFF
                            .checked_mul(multiplier)
                            .unwrap_or(MAXIMUM_RETRY_BACKOFF)
                            .min(MAXIMUM_RETRY_BACKOFF);
                        session.retry_not_before = Some(Instant::now() + backoff);
                    }
                }
                Err(error) => {
                    session.permanent_flush_error = Some(error.to_string());
                    latched_permanent_failure = true;
                    let _previous = session.flush_status.send_replace(FlushStatus {
                        durable_seq: session.durable_seq,
                        permanent_error: session.permanent_flush_error.clone(),
                    });
                }
            }
            if enqueue_after_commit {
                let next = session.oldest_claimable().cloned();
                let _ = session;
                if let Some(turn_id) = next {
                    enqueue(&mut state, session_id.clone(), turn_id);
                    claim_available = true;
                }
            }
            for turn_id in &pruned_turns {
                state.queued.remove(&(session_id.clone(), turn_id.clone()));
            }
            if !pruned_turns.is_empty() {
                state.claim_queue.retain(|(queued_session, queued_turn)| {
                    queued_session != session_id || !pruned_turns.contains(queued_turn)
                });
            }
            if evict_session {
                state.sessions.remove(session_id);
            }
        }
        if !pruned_turns.is_empty() {
            self.inner.settlement_requested.notify_one();
        }
        if claim_available {
            self.inner.claim_changed.notify_waiters();
        }
        if released_process_capacity || latched_permanent_failure {
            self.inner.process_pending_changed.notify_waiters();
        }
        if request_more {
            self.inner.flush_requested.notify_one();
        }
    }

    pub(super) async fn flush_every_session(&self) -> Result<()> {
        let targets = {
            let state = lock_state(&self.inner);
            state
                .sessions
                .iter()
                .map(|(session_id, session)| {
                    session
                        .live_seq()
                        .map(|seq| (session_id.clone(), session.flush_status.subscribe(), seq))
                })
                .collect::<Result<Vec<_>>>()?
        };
        self.inner.flush_requested.notify_one();
        let mut failures = Vec::new();
        for (session_id, status, through_seq) in targets {
            if let Err(error) = self.wait_on_flush_status(status, through_seq).await {
                failures.push(format!("{}: {error}", session_id.as_str()));
            }
        }
        if !failures.is_empty() {
            let count = failures.len();
            let first = failures.remove(0);
            return Err(KernelError::Shutdown(format!(
                "{count} session flush(es) failed; first failure: {first}"
            )));
        }
        Ok(())
    }

    pub(super) async fn wait_for_durable(
        &self,
        session_id: &SessionId,
        through_seq: u64,
    ) -> Result<u64> {
        let status = {
            let state = lock_state(&self.inner);
            flush_status_receiver(&state, session_id)?
        };
        self.inner.flush_requested.notify_one();
        self.wait_on_flush_status(status, through_seq).await
    }

    pub(super) async fn wait_on_flush_status(
        &self,
        mut status: watch::Receiver<FlushStatus>,
        through_seq: u64,
    ) -> Result<u64> {
        let deadline = Instant::now() + DURABILITY_WAIT_TIMEOUT;
        loop {
            let current = status.borrow().clone();
            if current.durable_seq >= through_seq {
                return Ok(current.durable_seq);
            }
            if let Some(error) = current.permanent_error {
                return Err(KernelError::Flush(error));
            }
            tokio::select! {
                changed = status.changed() => {
                    changed.map_err(|_| KernelError::Shutdown("flush status closed".into()))?;
                }
                () = self.inner.stop_worker.cancelled() => {
                    return Err(KernelError::Shutdown("flush worker stopped".into()));
                }
                () = tokio::time::sleep_until(deadline) => {
                    return Err(KernelError::Flush(format!(
                        "durability wait timed out after {} seconds",
                        DURABILITY_WAIT_TIMEOUT.as_secs()
                    )));
                }
            }
        }
    }

    /// Retries one atomic Agent commit after draining only resident, Fact-less
    /// append targets whose write-behind suffix advanced the Store CAS.
    ///
    /// Callers hold submission admission for every appended Session, so the
    /// resident live tail cannot grow while the retry drains and refreshes it.
    pub(super) async fn commit_agent_with_flush_conflict_retry(
        &self,
        mut commit: AtomicAgentCommit,
    ) -> TurnResult<std::result::Result<AtomicAgentCommitResult, StoreError>> {
        let first = self.inner.store.commit_agent(commit.clone()).await;
        let Err(conflict @ StoreError::Conflict { .. }) = first else {
            return Ok(first);
        };

        let mut refreshed = false;
        for append in &mut commit.sessions {
            if !append.facts.is_empty() || append.header.is_some() {
                continue;
            }
            let live_seq = {
                let state = lock_state(&self.inner);
                state
                    .sessions
                    .get(&append.session_id)
                    .map(SessionRuntime::live_seq)
                    .transpose()
                    .map_err(turn_kernel_error)?
            };
            let Some(live_seq) = live_seq.filter(|seq| *seq > append.expected_fact_seq) else {
                continue;
            };
            append.expected_fact_seq = self
                .wait_for_durable(&append.session_id, live_seq)
                .await
                .map_err(turn_kernel_error)?;
            refreshed = true;
        }
        if !refreshed {
            return Ok(Err(conflict));
        }
        Ok(self.inner.store.commit_agent(commit).await)
    }

    async fn read_ready_roots(
        &self,
        after: Option<&SessionId>,
    ) -> TurnResult<Option<rsi_agent_store_protocol::StoreReadyRootPage>> {
        match self
            .inner
            .store
            .list_ready_roots(after, MAXIMUM_ACTIVE_SESSIONS)
            .await
        {
            Ok(page) => {
                page.validate().map_err(turn_store_error)?;
                Ok(Some(page))
            }
            Err(StoreError::Io(_)) => Ok(None),
            Err(error) => Err(turn_store_error(error)),
        }
    }

    pub(super) async fn activate_one_ready_message(&self) -> TurnResult<bool> {
        // Several executor lanes may ask for work concurrently. Selection and the
        // following atomic message claim form one scheduler decision; serializing
        // only that decision prevents normal contention from escaping as a claim
        // error while Turn execution remains fully concurrent.
        let mut selection = self.inner.ready_activation.lock().await;
        let Some(mut roots) = self.read_ready_roots(selection.as_ref()).await? else {
            return Ok(false);
        };
        if roots.roots.is_empty() && selection.is_some() {
            let Some(first) = self.read_ready_roots(None).await? else {
                return Ok(false);
            };
            roots = first;
        }
        *selection = if roots.has_more {
            roots.roots.last().cloned()
        } else {
            None
        };
        for root_session_id in roots.roots {
            // A bad root is isolated to its own bounded scan. It must neither
            // terminate the shared executor generation nor hide later roots.
            match self.activate_ready_root(&root_session_id).await {
                Ok(true) => {
                    *selection = Some(root_session_id);
                    return Ok(true);
                }
                Err(error @ TurnError::Invariant(_)) => return Err(error),
                Ok(false) | Err(_) => {}
            }
        }
        Ok(false)
    }

    #[allow(clippy::too_many_lines)] // One root's eligibility scan and atomic claim form one scheduler decision.
    pub(super) async fn activate_ready_root(
        &self,
        root_session_id: &SessionId,
    ) -> TurnResult<bool> {
        // Validate the bounded durable tree before considering its ready entries.
        let _ = descendant_session_ids(&self.inner.store, root_session_id).await?;
        if lock_state(&self.inner)
            .tree_lanes
            .get(root_session_id)
            .and_then(Weak::upgrade)
            .is_some_and(|pool| pool.available_permits() == 0)
        {
            return Ok(false);
        }
        let mut after = None;
        let mut claimable_sessions = BTreeMap::new();
        loop {
            let ready = self
                .inner
                .store
                .list_ready_messages(root_session_id, after.as_ref(), MAXIMUM_SESSIONS_PER_READ)
                .await
                .map_err(turn_store_error)?;
            ready.validate().map_err(turn_store_error)?;
            for message in &ready.messages {
                if message.target != MessageTarget::NextTurn {
                    return Err(TurnError::Invariant(
                        "ready index contains a waking next-Step message".into(),
                    ));
                }
                let claimable = if let Some(claimable) = claimable_sessions.get(&message.session_id)
                {
                    *claimable
                } else {
                    let no_open_turn = self
                        .inner
                        .store
                        .list_open_turns(&message.session_id, 0, 1)
                        .await
                        .map_err(turn_store_error)?
                        .turns
                        .is_empty();
                    let no_active_activation = self
                        .inner
                        .store
                        .active_activation(&message.session_id)
                        .await
                        .map_err(turn_store_error)?
                        .is_none();
                    let claimable = no_open_turn && no_active_activation;
                    claimable_sessions.insert(message.session_id.clone(), claimable);
                    claimable
                };
                if !claimable {
                    continue;
                }
                let header = read_header_bounded(&self.inner, &message.session_id)
                    .await
                    .map_err(turn_store_error)?;
                let path = agent_root_and_path(&header).1;
                let suffix = message.control_seq;
                self.claim_message(ClaimMessage {
                    session: self.prepare_resume(&message.session_id).await?,
                    message_id: message.message_id.clone(),
                    activation_id: rsi_agent_session_protocol::ActivationId::new(format!(
                        "activation-{suffix}"
                    ))
                    .map_err(|error| TurnError::Invalid(error.to_string()))?,
                    path,
                    turn_id: TurnId::new(format!("turn-message-{suffix}"))
                        .map_err(|error| TurnError::Invalid(error.to_string()))?,
                    step_id: rsi_agent_session_protocol::StepId::new(format!(
                        "step-message-{suffix}"
                    ))
                    .map_err(|error| TurnError::Invalid(error.to_string()))?,
                })
                .await?;
                return Ok(true);
            }
            if !ready.has_more {
                return Ok(false);
            }
            after = Some(
                ready
                    .messages
                    .last()
                    .ok_or_else(|| {
                        TurnError::Invariant("ready-message pagination made no progress".into())
                    })?
                    .cursor(),
            );
        }
    }

    pub(super) fn validate_claim<'a>(
        &self,
        state: &'a KernelState,
        claim: &TurnClaim,
    ) -> TurnResult<&'a TurnControl> {
        if !self.inner.claim_issuer.validates(claim) {
            return Err(TurnError::StaleClaim);
        }
        let registration_id = state
            .executors
            .get(claim.executor_id())
            .copied()
            .ok_or(TurnError::StaleClaim)?;
        let session = state
            .sessions
            .get(claim.session_id())
            .ok_or(TurnError::StaleClaim)?;
        let turn = session
            .turns
            .get(claim.turn_id())
            .ok_or(TurnError::StaleClaim)?;
        match &turn.claim {
            Some(owner)
                if owner.executor == claim.executor_id()
                    && owner.registration == registration_id
                    && owner.claim == claim.claim_id()
                    && owner.live_seq == claim.live_seq()
                    && turn.accepted_at_ms == claim.accepted_at_ms()
                    && turn.accepted_seq == claim.accepted_seq()
                    && self
                        .inner
                        .claim_issuer
                        .validates_header(claim, &session.header) =>
            {
                Ok(turn)
            }
            _ => Err(TurnError::StaleClaim),
        }
    }

    pub(super) fn validate_agent_caller(&self, caller: &AgentCallerAuthority) -> TurnResult<()> {
        let state = lock_state(&self.inner);
        self.validate_claim(&state, caller.claim()).map(|_| ())
    }

    pub(super) fn validate_issued_claim(&self, claim: &TurnClaim) -> TurnResult<()> {
        self.inner
            .claim_issuer
            .validates(claim)
            .then_some(())
            .ok_or(TurnError::StaleClaim)
    }

    pub(super) async fn reserve_fresh_session(
        &self,
        header: &SessionHeader,
        durable_absence_known: bool,
    ) -> TurnResult<FreshReservationGuard> {
        let session_id = header.session_id();
        {
            let mut state = lock_state(&self.inner);
            if !state.accepting {
                return Err(TurnError::ShuttingDown);
            }
            if state.sessions.contains_key(session_id)
                || state.loading_sessions.contains_key(session_id)
                || state.fresh_reservations.contains(session_id)
            {
                return Err(TurnError::Invalid(
                    "fresh submission selected an existing or reserved session".into(),
                ));
            }
            if state
                .sessions
                .len()
                .saturating_add(state.loading_sessions.len())
                .saturating_add(state.fresh_reservations.len())
                >= MAXIMUM_ACTIVE_SESSIONS
            {
                return Err(TurnError::Capacity);
            }
            state.fresh_reservations.insert(session_id.clone());
        }
        let reservation = FreshReservationGuard::new(&self.inner, session_id.clone());
        if durable_absence_known {
            return Ok(reservation);
        }
        match read_header_bounded(&self.inner, session_id).await {
            Err(StoreError::NotFound(_)) => Ok(reservation),
            Ok(_) => Err(TurnError::Invalid(
                "fresh submission selected an existing session".into(),
            )),
            Err(error) => Err(turn_store_error(error)),
        }
    }

    pub(super) async fn prepare_resume_session(
        &self,
        session_id: &SessionId,
    ) -> TurnResult<PreparedResumeSession> {
        loop {
            let concurrent_load = {
                let state = lock_state(&self.inner);
                if !state.accepting {
                    return Err(TurnError::ShuttingDown);
                }
                if let Some(session) = state.sessions.get(session_id) {
                    return self
                        .inner
                        .resume_issuer
                        .issue(session.header.as_ref().clone(), session.composition.clone());
                }
                if state.fresh_reservations.contains(session_id) {
                    return Err(TurnError::Invalid(
                        "resume selected a session that is still being created".into(),
                    ));
                }
                state.loading_sessions.get(session_id).cloned()
            };
            if let Some(load) = concurrent_load {
                load.wait().await?;
                continue;
            }

            let header = read_header_bounded(&self.inner, session_id)
                .await
                .map_err(turn_store_error)?;
            let composition = match self.inner.composition.pin(header.agent_preset_id()).await {
                Ok(composition) => composition,
                Err(error) => {
                    let concurrent_load = {
                        let state = lock_state(&self.inner);
                        if !state.accepting {
                            return Err(TurnError::ShuttingDown);
                        }
                        if let Some(session) = state.sessions.get(session_id) {
                            return self.inner.resume_issuer.issue(
                                session.header.as_ref().clone(),
                                session.composition.clone(),
                            );
                        }
                        if state.fresh_reservations.contains(session_id) {
                            return Err(TurnError::Invalid(
                                "resume selected a session that is still being created".into(),
                            ));
                        }
                        state.loading_sessions.get(session_id).cloned()
                    };
                    if let Some(load) = concurrent_load {
                        load.wait().await?;
                        continue;
                    }
                    return Err(turn_composition_error(error));
                }
            };

            let concurrent_load = {
                let state = lock_state(&self.inner);
                if !state.accepting {
                    return Err(TurnError::ShuttingDown);
                }
                if let Some(session) = state.sessions.get(session_id) {
                    return self
                        .inner
                        .resume_issuer
                        .issue(session.header.as_ref().clone(), session.composition.clone());
                }
                if state.fresh_reservations.contains(session_id) {
                    return Err(TurnError::Invalid(
                        "resume selected a session that is still being created".into(),
                    ));
                }
                state.loading_sessions.get(session_id).cloned()
            };
            if let Some(load) = concurrent_load {
                load.wait().await?;
                continue;
            }
            return self.inner.resume_issuer.issue(header, composition);
        }
    }

    pub(super) async fn ensure_prepared_session_loaded(
        &self,
        prepared: &PreparedResumeSession,
    ) -> TurnResult<()> {
        let (header, composition) = self.inner.resume_issuer.inspect(prepared)?;
        let header = header.clone();
        let composition = composition.clone();
        let session_id = header.session_id().clone();
        let (load, leader) = {
            let mut state = lock_state(&self.inner);
            if !state.accepting {
                return Err(TurnError::ShuttingDown);
            }
            if state.sessions.contains_key(&session_id) {
                return Ok(());
            }
            if state.fresh_reservations.contains(&session_id) {
                return Err(TurnError::Invalid(
                    "resume selected a session that is still being created".into(),
                ));
            }
            if let Some(load) = state.loading_sessions.get(&session_id) {
                (Arc::clone(load), false)
            } else {
                if state
                    .sessions
                    .len()
                    .saturating_add(state.loading_sessions.len())
                    .saturating_add(state.fresh_reservations.len())
                    >= MAXIMUM_ACTIVE_SESSIONS
                {
                    return Err(TurnError::Capacity);
                }
                let load = Arc::new(SessionLoad::pending());
                state
                    .loading_sessions
                    .insert(session_id.clone(), Arc::clone(&load));
                (load, true)
            }
        };
        if !leader {
            return load.wait().await;
        }
        let load_guard = SessionLoadGuard::new(&self.inner, session_id.clone(), Arc::clone(&load));
        let budget = header.settings().turn_budget().clone();
        let loaded = load_control_state(&self.inner.store, Some(&self.inner), &session_id, &budget)
            .await
            .map_err(turn_kernel_error);
        let result = {
            let mut state = lock_state(&self.inner);
            if !state.accepting {
                Err(TurnError::ShuttingDown)
            } else if state.sessions.contains_key(&session_id) {
                Ok(())
            } else {
                match loaded {
                    Err(error) => Err(error),
                    Ok((durable_seq, turns, turn_order, workspace_context)) => {
                        let mut session =
                            SessionRuntime::new(header, composition, durable_seq, false);
                        session.turns = turns;
                        session.turn_order = turn_order;
                        session.workspace_context = workspace_context;
                        let queued = session.turn_order.clone();
                        state.sessions.insert(session_id.clone(), session);
                        for turn_id in queued {
                            enqueue(&mut state, session_id.clone(), turn_id);
                        }
                        Ok(())
                    }
                }
            }
        };
        load_guard.complete(result.clone());
        if result.is_ok() {
            self.inner.claim_changed.notify_waiters();
        }
        result
    }

    pub(super) async fn ensure_session_loaded(&self, session_id: &SessionId) -> TurnResult<()> {
        let prepared = self.prepare_resume_session(session_id).await?;
        self.ensure_prepared_session_loaded(&prepared).await
    }

    pub(super) async fn reserve_resume_submission(
        &self,
        prepared: &PreparedResumeSession,
    ) -> TurnResult<ResumeAdmissionGuard> {
        let (header, _) = self.inner.resume_issuer.inspect(prepared)?;
        let session_id = header.session_id().clone();
        loop {
            self.ensure_prepared_session_loaded(prepared).await?;
            let mut state = lock_state(&self.inner);
            let Some(session) = state.sessions.get_mut(&session_id) else {
                continue;
            };
            session.admission_reservations = session
                .admission_reservations
                .checked_add(1)
                .ok_or_else(|| {
                    TurnError::Invariant("resume admission reservation count overflowed".into())
                })?;
            return Ok(ResumeAdmissionGuard {
                inner: Arc::clone(&self.inner),
                session_id,
            });
        }
    }

    pub(super) fn accept_turn(
        &self,
        session_selection: SubmitSession,
        turn_id: TurnId,
        body: SessionFactBody,
    ) -> TurnResult<SubmittedTurn> {
        body.validate()
            .map_err(|error| TurnError::Invalid(error.to_string()))?;
        let session_id = session_selection.session_id().clone();
        let mut state = lock_state(&self.inner);
        if !state.accepting {
            if matches!(&session_selection, SubmitSession::Fresh(_)) {
                state.fresh_reservations.remove(&session_id);
            }
            return Err(TurnError::ShuttingDown);
        }
        let inserted_fresh = matches!(&session_selection, SubmitSession::Fresh(_));
        match session_selection {
            SubmitSession::Fresh(prepared) => {
                let (header, composition) = prepared.into_parts();
                if state.sessions.contains_key(&session_id)
                    || !state.fresh_reservations.remove(&session_id)
                {
                    return Err(TurnError::Invalid(
                        "fresh submission lacks its exact resident reservation".into(),
                    ));
                }
                state.sessions.insert(
                    session_id.clone(),
                    SessionRuntime::new(header, composition, 0, true),
                );
            }
            SubmitSession::Resume(prepared) => {
                let _parts = self.inner.resume_issuer.consume(prepared)?;
                if !state.sessions.contains_key(&session_id) {
                    return Err(TurnError::SessionNotFound(session_id.to_string()));
                }
            }
        }
        let staged = (|| {
            let session = state
                .sessions
                .get_mut(&session_id)
                .expect("fresh was inserted and resume was checked");
            if let Some(error) = &session.permanent_flush_error {
                return Err(TurnError::Flush(error.clone()));
            }
            let live_turns = session
                .turns
                .values()
                .filter(|turn| turn.terminal.is_none())
                .count();
            if live_turns >= MAXIMUM_LIVE_TURNS {
                return Err(TurnError::Capacity);
            }
            if session.turns.contains_key(&turn_id) {
                return Err(TurnError::Invariant(
                    "duplicate turn identity escaped submission retry handling".into(),
                ));
            }
            let fact = next_fact(&self.inner, session, body).map_err(turn_kernel_error)?;
            push_pending(&self.inner, session, fact.clone()).map_err(turn_kernel_error)?;
            Ok(fact)
        })();
        let fact = match staged {
            Ok(fact) => fact,
            Err(error) => {
                if inserted_fresh {
                    state.sessions.remove(&session_id);
                }
                return Err(error);
            }
        };
        let accepted_seq = fact.seq();
        let session = state
            .sessions
            .get_mut(&session_id)
            .expect("accepted session exists");
        session.turns.insert(
            turn_id.clone(),
            TurnControl::new(fact.timestamp_ms(), accepted_seq),
        );
        session.turn_order.push(turn_id.clone());
        publish_live_watermarks(session);
        enqueue(&mut state, session_id.clone(), turn_id.clone());
        drop(state);
        self.inner.claim_changed.notify_waiters();
        Ok(SubmittedTurn {
            session_id,
            turn_id,
            accepted_seq,
        })
    }

    pub(super) async fn existing_submission(
        &self,
        header: &SessionHeader,
        turn_id: &TurnId,
        body: &SessionFactBody,
        header_is_durable: bool,
    ) -> TurnResult<(Option<(SubmittedTurn, bool)>, bool)> {
        let session_id = header.session_id();
        {
            let state = lock_state(&self.inner);
            if let Some(session) = state.sessions.get(session_id)
                && let Some(turn) = session.turns.get(turn_id)
            {
                if session.header.as_ref() != header {
                    return Err(submission_conflict(session_id, turn_id));
                }
                if turn.accepted_seq > session.durable_seq {
                    let accepted = session
                        .pending
                        .iter()
                        .find(|fact| fact.seq() == turn.accepted_seq)
                        .ok_or_else(|| {
                            TurnError::Invariant(
                                "live submission acceptance is absent from the pending suffix"
                                    .into(),
                            )
                        })?;
                    if accepted.body() != body {
                        return Err(submission_conflict(session_id, turn_id));
                    }
                    return Ok((
                        Some((
                            SubmittedTurn {
                                session_id: session_id.clone(),
                                turn_id: turn_id.clone(),
                                accepted_seq: turn.accepted_seq,
                            },
                            true,
                        )),
                        true,
                    ));
                }
            } else if state.sessions.contains_key(session_id) {
                // A resident session may have pruned this turn's durable
                // terminal entry; fall through to the indexed Store read.
            }
        }

        if !header_is_durable {
            let durable_header = match read_header_bounded(&self.inner, session_id).await {
                Ok(header) => header,
                Err(StoreError::NotFound(_)) => return Ok((None, false)),
                Err(error) => return Err(turn_store_error(error)),
            };
            if &durable_header != header {
                return Err(submission_conflict(session_id, turn_id));
            }
        }
        let boundary = match read_turn_boundary_bounded(&self.inner, session_id, turn_id).await {
            Ok(boundary) => boundary,
            Err(StoreError::NotFound(_) | StoreError::TurnNotFound { .. }) => {
                return Ok((None, true));
            }
            Err(error) => return Err(turn_store_error(error)),
        };
        let accepted_seq = boundary.accepted_seq();
        let (_, accepted, _, _) = boundary.into_parts();
        if accepted.body() != body {
            return Err(submission_conflict(session_id, turn_id));
        }
        Ok((
            Some((
                SubmittedTurn {
                    session_id: session_id.clone(),
                    turn_id: turn_id.clone(),
                    accepted_seq,
                },
                false,
            )),
            true,
        ))
    }

    pub(super) async fn submit_body(
        &self,
        session: SubmitSession,
        turn_id: TurnId,
        body: SessionFactBody,
    ) -> TurnResult<SubmittedTurn> {
        body.validate()
            .map_err(|error| TurnError::Invalid(error.to_string()))?;
        if let SubmitSession::Resume(prepared) = &session {
            self.inner.resume_issuer.inspect(prepared)?;
        }
        let header = session.header();
        let submission_admission = self
            .inner
            .submission_admission
            .acquire(header.session_id())
            .await?;
        let header_is_durable = matches!(&session, SubmitSession::Resume(_));
        let (existing, durable_session_exists) = self
            .existing_submission(header, &turn_id, &body, header_is_durable)
            .await?;
        if let Some((receipt, pending)) = existing {
            drop(submission_admission);
            if pending {
                self.wait_for_durable(&receipt.session_id, receipt.accepted_seq)
                    .await
                    .map_err(turn_kernel_error)?;
            }
            return Ok(receipt);
        }
        let resume_admission = match &session {
            SubmitSession::Fresh(_) => None,
            SubmitSession::Resume(prepared) => {
                Some(self.reserve_resume_submission(prepared).await?)
            }
        };
        let fresh_reservation = match &session {
            SubmitSession::Fresh(prepared) => Some(
                self.reserve_fresh_session(prepared.header(), !durable_session_exists)
                    .await?,
            ),
            SubmitSession::Resume(_) => None,
        };
        let result = self.accept_turn(session, turn_id, body);
        drop(fresh_reservation);
        drop(resume_admission);
        drop(submission_admission);
        let receipt = result?;
        self.wait_for_durable(&receipt.session_id, receipt.accepted_seq)
            .await
            .map_err(turn_kernel_error)?;
        Ok(receipt)
    }
}

pub(super) fn flush_status_receiver(
    state: &KernelState,
    session_id: &SessionId,
) -> Result<watch::Receiver<FlushStatus>> {
    if let Some(session) = state.sessions.get(session_id) {
        return Ok(session.flush_status.subscribe());
    }
    if !state.accepting {
        return Err(KernelError::Shutdown(
            "session was released while the Kernel was shutting down".into(),
        ));
    }
    Err(KernelError::Invariant(
        "session disappeared during flush".into(),
    ))
}
