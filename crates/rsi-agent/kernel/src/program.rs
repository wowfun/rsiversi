//! Independent live Program ownership over canonical, bounded run controls.
use super::*;
use rsi_agent_session_protocol::{
    ActivationId, ExecutionOwner, ProgramBlob, ProgramChildReceipt, ProgramForkBoundary,
    ProgramOutcome, ProgramResultBinding, ProgramRunDescriptor, ProgramRunEvent, ProgramRunId,
};
use rsi_agent_turn_protocol::{
    PrepareProgram, ProgramAgentRequest, ProgramAgentResult, ProgramRun,
};
mod children;
mod notice;
mod owner;
mod reconciliation;
mod state;
mod view;
use state::RunState;

// Process-local owners retain Jobs, scripts and frozen composition pins.
const MAXIMUM_LIVE_PROGRAMS: usize = 8;

pub(super) enum CompletionRoute {
    NotOwned,
    Append(Box<AtomicSessionAppend>),
    Interrupted,
}

#[derive(Debug)]
pub(super) struct LiveRun {
    kernel: AgentKernel,
    descriptor: ProgramRunDescriptor,
    script: Arc<[u8]>,
    state: AsyncMutex<Option<(rsi_agent_store_protocol::StoreProgramHead, Arc<RunState>)>>,
    header: SessionHeader,
    composition: AgentCompositionPin,
    baseline: rsi_agent_composition_protocol::DomainBaseline,
    cancellation: CancellationToken,
    creator_cancellation: CancellationToken,
    turn_cancellation: CancellationToken,
}
impl Drop for LiveRun {
    fn drop(&mut self) {
        self.cancellation.cancel();
    }
}
fn invalid(message: &str) -> TurnError {
    TurnError::Invalid(message.into())
}
fn session_error(error: impl fmt::Display) -> TurnError {
    invalid(&error.to_string())
}
impl AgentKernel {
    pub(super) async fn prepare_program_run(
        &self,
        request: PrepareProgram,
    ) -> TurnResult<Arc<dyn ProgramRun>> {
        let (effect, turn_cancellation, sandbox, require_approval) =
            self.program_creator_authority(&request).await?;
        let composition = self.composition(request.caller.claim())?;
        let header = request.caller.header().clone();
        let boundary = self
            .inner
            .store
            .resolve_fork_boundary(
                header.session_id(),
                request.caller.turn_id(),
                request.fork_turns.clone(),
            )
            .await
            .map_err(turn_store_error)?;
        let script: Arc<[u8]> = Arc::from(request.script.into_bytes());
        let run_id = ProgramRunId::new(format!(
            "program-{:x}",
            Sha256::digest(
                serde_json::to_vec(&(header.session_id(), request.caller.turn_id(), &effect))
                    .map_err(session_error)?
            )
        ))
        .map_err(session_error)?;
        let descriptor = ProgramRunDescriptor {
            run_id,
            session_id: header.session_id().clone(),
            creator_turn_id: request.caller.turn_id().clone(),
            creator_effect_id: effect,
            parent_header_sha256: header.fingerprint().map_err(session_error)?,
            fork: ProgramForkBoundary {
                requested_turns: request.fork_turns,
                resolved_after_seq: boundary.resolved_after_seq,
                resolved_terminal_seq: boundary.resolved_terminal_seq,
                terminal_prefix_sha256: boundary.terminal_prefix_sha256,
                resolved_terminal_control_seq: boundary.resolved_terminal_control_seq,
                terminal_control_prefix_sha256: boundary.terminal_control_prefix_sha256,
                effective_turns: boundary.effective_turns,
            },
            selection: request
                .caller
                .source_selection()
                .ok_or_else(|| invalid("program creator selection is absent"))?
                .clone(),
            sandbox,
            require_approval,
            script: ProgramBlob {
                sha256: format!("{:x}", Sha256::digest(&script)),
                bytes: script.len() as u64,
            },
            guard: request.guard,
        };
        descriptor.validate().map_err(session_error)?;
        let baseline = self
            .program_baseline(&header, &composition, &descriptor)
            .await?;
        let _admission = self
            .inner
            .submission_admission
            .acquire(header.session_id())
            .await?;
        self.validate_agent_caller(&request.caller)?;
        self.validate_program_guard(&descriptor).await?;
        let run = Arc::new(LiveRun {
            kernel: self.clone(),
            descriptor,
            script,
            state: AsyncMutex::new(None),
            header,
            composition,
            baseline,
            cancellation: CancellationToken::new(),
            creator_cancellation: request.cancellation,
            turn_cancellation,
        });
        {
            let mut registry = self
                .inner
                .programs
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            registry.retain(|_, run| run.strong_count() > 0);
            if registry.len() >= MAXIMUM_LIVE_PROGRAMS
                || registry.contains_key(run.header.session_id())
            {
                return Err(TurnError::Capacity);
            }
            registry.insert(run.header.session_id().clone(), Arc::downgrade(&run));
        }
        Ok(run)
    }
    async fn program_baseline(
        &self,
        header: &SessionHeader,
        composition: &AgentCompositionPin,
        descriptor: &ProgramRunDescriptor,
    ) -> TurnResult<rsi_agent_composition_protocol::DomainBaseline> {
        let mut baseline = PreparedFreshSession::new(header.clone(), composition.clone())
            .map_err(turn_composition_error)?
            .baseline()
            .clone();
        if descriptor.fork.resolved_terminal_control_seq > 0 {
            let inherited = observation::read_domain_states_bounded(
                &self.inner,
                header.session_id(),
                Some(descriptor.fork.resolved_terminal_control_seq),
            )
            .await
            .map_err(turn_store_error)?;
            baseline
                .inherit(
                    &inherited
                        .states
                        .into_iter()
                        .map(|state| state.snapshot)
                        .collect::<Vec<_>>(),
                )
                .map_err(session_error)?;
        }
        Ok(baseline)
    }
    async fn program_creator_authority(
        &self,
        request: &PrepareProgram,
    ) -> TurnResult<(EffectId, CancellationToken, rsi_sandbox::SandboxMode, bool)> {
        self.validate_agent_caller(&request.caller)?;
        if request.caller.header().fork_origin().is_some()
            || request.script.is_empty()
            || request.script.len() > rsi_agent_session_protocol::MAXIMUM_PROGRAM_SCRIPT_BYTES
            || request.continuation_domains.len()
                > rsi_agent_session_protocol::MAXIMUM_PROGRAM_CONTINUATION_DOMAINS
        {
            return Err(invalid("program creation requires bounded root authority"));
        }
        let effect = request
            .caller
            .tool_effect_id()
            .ok_or_else(|| invalid("program creation requires a started Tool"))?
            .clone();
        let turn_cancellation = {
            let state = lock_state(&self.inner);
            let turn = self.validate_claim(&state, request.caller.claim())?;
            if !matches!(
                turn.effects.get(&effect),
                Some(ActiveEffect::Tool {
                    started: true,
                    program_role: rsi_tools_protocol::ToolProgramRole::Workflow,
                    origin: rsi_agent_session_protocol::ToolOrigin::Model { .. },
                    ..
                })
            ) {
                return Err(invalid(
                    "program creation requires its exact model-origin Workflow Tool",
                ));
            }
            turn.cancellation.clone()
        };
        let input =
            rsi_agent_turn_protocol::initial_turn_input(self, request.caller.claim()).await?;
        if !input.human
            && !input
                .continuation_domains
                .iter()
                .any(|domain| request.continuation_domains.contains(domain))
        {
            return Err(invalid(
                "program creation requires initial human input or an allowed finite automatic round",
            ));
        }
        let sandbox = input.sandbox;
        let require_approval = input.require_approval;
        Ok((effect, turn_cancellation, sandbox, require_approval))
    }
    async fn validate_program_guard(&self, descriptor: &ProgramRunDescriptor) -> TurnResult<()> {
        let Some(guard) = &descriptor.guard else {
            return Ok(());
        };
        let page =
            observation::read_domain_states_bounded(&self.inner, &descriptor.session_id, None)
                .await
                .map_err(turn_store_error)?;
        if !page.states.iter().any(|state| {
            state.head.revision == guard.revision
                && state.snapshot.identity() == &guard.domain
                && state
                    .snapshot
                    .sha256()
                    .is_ok_and(|sha| sha == guard.snapshot_sha256)
        }) {
            return Err(invalid("program policy generation has changed"));
        }
        Ok(())
    }
    async fn read_program_state(
        &self,
        session: &SessionId,
        run: &ProgramRunId,
    ) -> TurnResult<Arc<RunState>> {
        let live = self
            .inner
            .programs
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(session)
            .and_then(Weak::upgrade)
            .filter(|live| live.run_id() == run);
        if let Some(live) = live {
            let mut cached = live.state.lock().await;
            self.refresh_program_state(session, run, &mut cached).await
        } else {
            self.refresh_program_state(session, run, &mut None).await
        }
    }
    async fn read_program_suffix(
        &self,
        session: &SessionId,
        run: &ProgramRunId,
        after: u64,
    ) -> std::result::Result<
        store_reads::Materialized<Option<rsi_agent_store_protocol::StoreProgramRecords>>,
        StoreError,
    > {
        let run = run.clone();
        store_reads::read(
            &self.inner,
            session,
            usize::try_from(rsi_agent_session_protocol::MAXIMUM_PROGRAM_RECORD_BYTES)
                .expect("8 MiB fits supported address spaces"),
            true,
            move |store, session| async move {
                store
                    .read_program_records_after(&session, &run, after)
                    .await
            },
        )
        .await
    }
    async fn refresh_program_state(
        &self,
        session: &SessionId,
        run: &ProgramRunId,
        cached: &mut Option<(rsi_agent_store_protocol::StoreProgramHead, Arc<RunState>)>,
    ) -> TurnResult<Arc<RunState>> {
        let after = cached.as_ref().map_or(0, |(head, _)| head.last_control_seq);
        let (records, _bytes, _lease) = self
            .read_program_suffix(session, run, after)
            .await
            .map_err(turn_store_error)?;
        let records = records.ok_or_else(|| invalid("program run is not accepted"))?;
        records
            .validate_after(session, run, cached.as_ref().map(|(head, _)| head))
            .map_err(turn_store_error)?;
        // Remove the old cache only after validating the complete suffix. If a
        // semantic fold fails, a later read rebuilds from canonical history.
        let state = if let Some((_, mut state)) = cached.take() {
            if !records.records.is_empty() {
                let updated = Arc::make_mut(&mut state);
                for record in &records.records {
                    let AgentControlRecordBody::ProgramRun { event, .. } = record.body() else {
                        return Err(invalid("foreign Program record"));
                    };
                    updated.apply(event)?;
                }
                updated.control_seq = records.head.last_control_seq;
            }
            state
        } else {
            Arc::new(RunState::replay(&records)?)
        };
        *cached = Some((records.head, state.clone()));
        Ok(state)
    }
    /// Caller retains owning Session admission; no source Tool claim is needed.
    pub(super) async fn program_append(
        &self,
        session: &SessionId,
        run: &ProgramRunId,
        event: ProgramRunEvent,
    ) -> TurnResult<AtomicSessionAppend> {
        if !matches!(event, ProgramRunEvent::Accepted { .. }) {
            Arc::unwrap_or_clone(self.read_program_state(session, run).await?).apply(&event)?;
        }
        self.fence_pending_terminal(session).await?;
        let tail = self
            .inner
            .store
            .read_watermarks(session)
            .await
            .map_err(turn_store_error)?;
        let record = AgentControlRecord::new(
            tail.durable_control_seq + 1,
            self.inner.clock.now_ms().max(1),
            AgentControlRecordBody::ProgramRun {
                run_id: run.clone(),
                event,
            },
        )
        .map_err(session_error)?;
        Ok(AtomicSessionAppend {
            session_id: session.clone(),
            expected_fact_seq: tail.durable_fact_seq,
            expected_control_seq: tail.durable_control_seq,
            header: None,
            facts: vec![],
            controls: vec![record],
        })
    }
    async fn commit_program_append(&self, append: AtomicSessionAppend) -> TurnResult<()> {
        self.commit_agent_with_flush_conflict_retry(AtomicAgentCommit {
            sessions: vec![append],
            required_active_activations: vec![],
            quiescent_descendants_of: None,
        })
        .await?
        .map_err(turn_store_error)?;
        Ok(())
    }
}
impl LiveRun {
    pub(super) fn run_id(&self) -> &ProgramRunId {
        &self.descriptor.run_id
    }
    pub(super) fn pinned_composition(&self) -> AgentCompositionPin {
        self.composition.clone()
    }
    fn live(&self) -> TurnResult<()> {
        if self.cancellation.is_cancelled() || !lock_state(&self.kernel.inner).accepting {
            return Err(TurnError::Cancelled);
        }
        let registry = self
            .kernel
            .inner
            .programs
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if registry
            .get(&self.descriptor.session_id)
            .is_none_or(|weak| !std::ptr::eq(weak.as_ptr(), self))
        {
            return Err(TurnError::StaleClaim);
        }
        Ok(())
    }
    async fn state(&self) -> TurnResult<Arc<RunState>> {
        self.kernel
            .read_program_state(&self.descriptor.session_id, &self.descriptor.run_id)
            .await
    }
    async fn append(
        &self,
        event: ProgramRunEvent,
        admission: SubmissionAdmissionLease,
    ) -> TurnResult<()> {
        let append = self
            .kernel
            .program_append(&self.descriptor.session_id, &self.descriptor.run_id, event)
            .await?;
        let kernel = self.kernel.clone();
        self.kernel
            .owned_commit(async move {
                let _admission = admission;
                kernel.commit_program_append(append).await
            })
            .await
    }
    async fn active(&self) -> TurnResult<()> {
        self.live()?;
        self.kernel.validate_program_guard(&self.descriptor).await
    }
}
impl AgentKernel {
    pub(super) async fn recover_program_runs(&self) -> TurnResult<()> {
        let mut after = None;
        loop {
            let page = self
                .inner
                .store
                .list_active_program_runs(after.as_ref(), MAXIMUM_SESSIONS_PER_READ)
                .await
                .map_err(turn_store_error)?;
            if page.runs.len() > MAXIMUM_SESSIONS_PER_READ
                || page.runs.windows(2).any(|pair| pair[0] >= pair[1])
                || page
                    .runs
                    .first()
                    .is_some_and(|key| after.as_ref().is_some_and(|after| key <= after))
                || page.has_more && page.runs.is_empty()
            {
                return Err(invalid("invalid active program recovery page"));
            }
            for key in &page.runs {
                let state = self
                    .read_program_state(&key.session_id, &key.run_id)
                    .await?;
                // All accepted Turns were already repaired. Pending initial inputs must be
                // discarded before any ready worker can convert them to new provider work.
                for child in state
                    .children
                    .values()
                    .filter(|child| child.receipt.is_none())
                {
                    self.discard_program_branch_pending(&child.session_id)
                        .await?;
                    let header = read_validated_header_bounded(&self.inner, &child.session_id)
                        .await
                        .map_err(turn_store_error)?;
                    if child.activation.is_none() {
                        self.cancel_program_message(
                            &header,
                            &child.message_id,
                            ProgramOutcome::Interrupted,
                        )
                        .await?;
                    }
                }
                self.reconcile_waiting_activations()
                    .await
                    .map_err(turn_kernel_error)?;
                let _admission = self
                    .inner
                    .submission_admission
                    .acquire(&key.session_id)
                    .await?;
                let append = self
                    .program_append(
                        &key.session_id,
                        &key.run_id,
                        ProgramRunEvent::Terminal {
                            outcome: ProgramOutcome::Interrupted,
                            result: None,
                        },
                    )
                    .await?;
                self.commit_program_append(append).await?;
            }
            if !page.has_more {
                break;
            }
            after = page.runs.last().cloned();
        }
        Ok(())
    }
}
impl KernelInner {
    pub(super) fn revoke_program_guards(&self, changes: &[(SessionId, String)]) {
        let registry = self
            .programs
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        for (session, domain) in changes {
            if let Some(run) = registry.get(session).and_then(Weak::upgrade)
                && run
                    .descriptor
                    .guard
                    .as_ref()
                    .is_some_and(|guard| guard.domain.id() == domain)
            {
                run.cancellation.cancel();
            }
        }
    }
}
