//! Exact generation adoption at a settled request boundary.
use super::*;
use rsi_agent_composition_protocol::{AgentGenerationSeed, ToolOutputCatalog};
use rsi_agent_session_protocol::{
    DomainMutationSource, DomainStateCommit, DomainStateUpdate, ExecutionOwner,
};
use rsi_agent_turn_protocol::PreparedCompositionUpdate;

pub(super) const PREPARATION_BUDGET: std::time::Duration = std::time::Duration::from_secs(30);

pub(super) struct Attempt {
    selection: rsi_agent_session_protocol::GenerationSelection,
    domain_seq: u64,
    context_sensitive: bool,
    pub(super) turn_id: TurnId,
    pub(super) rejected: bool,
}

pub(super) fn narrow(
    claim: &TurnClaim,
    composition: AgentCompositionPin,
    output: Option<rsi_agent_session_protocol::OutputContract>,
) -> TurnResult<AgentCompositionPin> {
    let composition = composition.for_delegation(claim.header().delegation_policy());
    match output {
        Some(output) => composition
            .with_output_contract(
                &format!("{}:{}", claim.session_id(), claim.turn_id()),
                output,
            )
            .map_err(turn_composition_error),
        None => Ok(composition),
    }
}

struct PreparedGeneration {
    kernel: AgentKernel,
    caller: AgentCallerAuthority,
    lease: mutation::AgentMutationLease,
    previous: AgentCompositionPin,
    composition: AgentCompositionPin,
    claim_composition: AgentCompositionPin,
    append: AtomicSessionAppend,
}

impl fmt::Debug for PreparedGeneration {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PreparedGeneration")
            .field("selection", self.composition.selection())
            .finish_non_exhaustive()
    }
}

#[async_trait]
impl PreparedCompositionUpdate for PreparedGeneration {
    fn composition(&self) -> AgentCompositionPin {
        self.claim_composition.clone()
    }

    async fn commit(self: Box<Self>) -> TurnResult<AgentCompositionPin> {
        let kernel = self.kernel.clone();
        kernel
            .owned_commit(async move { self.publish().await })
            .await
    }
}

impl PreparedGeneration {
    async fn publish(self) -> TurnResult<AgentCompositionPin> {
        let Self {
            kernel,
            caller,
            lease,
            previous,
            composition,
            claim_composition,
            append,
        } = self;
        let _admission = kernel
            .inner
            .submission_admission
            .acquire(caller.session_id())
            .await?;
        lease.validate(&kernel, &caller)?;
        {
            let state = lock_state(&kernel.inner);
            let turn = kernel.validate_claim(&state, caller.claim())?;
            settled(turn)?;
            let session = &state.sessions[caller.session_id()];
            if session.live_seq().map_err(turn_kernel_error)? != append.expected_fact_seq
                || !session.composition.same_generation(&previous)
            {
                return Err(TurnError::Composition(
                    "candidate predecessor changed during preparation".into(),
                ));
            }
        }
        let expected_fact_seq = append.expected_fact_seq;
        let commit = AtomicAgentCommit {
            sessions: vec![append],
            required_active_activations: vec![],
            quiescent_descendants_of: None,
        };
        if let Err(error) = kernel.inner.commit_agent(commit.clone()).await {
            if matches!(
                error,
                StoreError::ControlConflict { .. } | StoreError::Conflict { .. }
            ) {
                return Err(TurnError::Composition(
                    "candidate predecessor changed during preparation".into(),
                ));
            }
            if !matches!(error, StoreError::Io(_)) {
                return Err(turn_store_error(error));
            }
            match kernel.inner.reconcile_commit(&commit).await {
                Ok(Some(receipt)) => kernel.inner.notify_committed(&receipt),
                Ok(None) => return Err(turn_store_error(error)),
                Err(_) => {
                    let unknown = TurnError::Store("generation publication requires recovery: acknowledgement and proof unavailable".into());
                    lease.fail_session(&unknown);
                    return Err(unknown);
                }
            }
        }
        let result = (|| {
            lease.validate(&kernel, &caller)?;
            let mut state = lock_state(&kernel.inner);
            let session = state
                .sessions
                .get_mut(caller.session_id())
                .ok_or(TurnError::StaleClaim)?;
            if session.durable_seq != expected_fact_seq
                || !session.pending.is_empty()
                || !session.composition.same_generation(&previous)
            {
                return Err(TurnError::Invariant(
                    "generation publication changed its resident predecessor".into(),
                ));
            }
            let turn = session
                .turns
                .get_mut(caller.turn_id())
                .ok_or(TurnError::StaleClaim)?;
            turn.program_roles = Arc::new(claim_composition.tools().program_roles());
            turn.claim_composition = Some(claim_composition.clone());
            // Existing request and background owners retain their independent old pins.
            session.composition = composition;
            session.generation_rejection = None;
            session.generation_attempt = None;
            Ok(claim_composition)
        })();
        if let Err(error) = &result {
            lease.fail_session(error);
        } else {
            kernel.inner.session_changes.committed(caller.session_id());
        }
        result
    }
}

impl AgentKernel {
    pub(super) async fn prepare_cold_generation(
        &self,
        header: &SessionHeader,
        composition: &AgentCompositionPin,
        expected_domain_seq: Option<u64>,
    ) -> TurnResult<Option<AtomicAgentCommit>> {
        let page = observation::read_domain_states_bounded(&self.inner, header.session_id(), None)
            .await
            .map_err(turn_store_error)?;
        if expected_domain_seq.is_some_and(|seq| seq != page.domain_control_seq) {
            return Err(TurnError::Composition(
                "Domain state changed after resume preparation; prepare again".into(),
            ));
        }
        if page
            .generation
            .as_ref()
            .and_then(|current| current.selection.preset())
            != composition.selection().preset()
        {
            return Err(TurnError::Composition(
                "Session preset changed since resume preparation".into(),
            ));
        }
        self.generation_commit(header, composition, &page).await
    }

    pub(super) async fn generation_commit(
        &self,
        header: &SessionHeader,
        composition: &AgentCompositionPin,
        page: &rsi_agent_store_protocol::StoreDomainStatePage,
    ) -> TurnResult<Option<AtomicAgentCommit>> {
        let seq = page
            .durable_control_seq
            .checked_add(1)
            .ok_or_else(|| TurnError::Invariant("control sequence exhausted".into()))?;
        let adoption = adoption_states(composition, &page.states, seq)?;
        validate_cold_state(header, composition, &adoption.states).await?;
        if page
            .generation
            .as_ref()
            .is_some_and(|selected| &selected.selection == composition.selection())
            && adoption.updates.is_empty()
            && adoption.removed.is_empty()
        {
            return Ok(None);
        }
        let commit = DomainStateCommit::new(
            None,
            DomainMutationSource::Recovery {
                selection: composition.selection().clone(),
            },
            adoption.updates,
        )
        .and_then(|commit| commit.with_removed(adoption.removed))
        .map_err(|error| TurnError::Composition(error.to_string()))?;
        let record = AgentControlRecord::new(
            seq,
            self.inner.clock.now_ms().max(1),
            AgentControlRecordBody::DomainStateCommitted { commit },
        )
        .map_err(|error| TurnError::Composition(error.to_string()))?;
        let commit = AtomicAgentCommit {
            sessions: vec![AtomicSessionAppend {
                session_id: header.session_id().clone(),
                expected_fact_seq: page.durable_fact_seq,
                expected_control_seq: page.durable_control_seq,
                header: None,
                facts: vec![],
                controls: vec![record],
            }],
            required_active_activations: vec![],
            quiescent_descendants_of: None,
        };
        Ok(Some(commit))
    }

    // The caller owns submission admission through publication and installation.
    pub(super) async fn publish_cold_generation(
        &self,
        commit: AtomicAgentCommit,
    ) -> TurnResult<()> {
        match self.inner.commit_agent(commit.clone()).await {
            Ok(_) => Ok(()),
            Err(StoreError::ControlConflict { .. } | StoreError::Conflict { .. }) => Err(
                TurnError::Composition("candidate predecessor changed during preparation".into()),
            ),
            Err(StoreError::Io(_)) => match self
                .inner
                .reconcile_commit(&commit)
                .await
                .map_err(turn_store_error)?
            {
                Some(receipt) => {
                    self.inner.notify_committed(&receipt);
                    Ok(())
                }
                None => Err(TurnError::Store(
                    "cold generation publication did not commit".into(),
                )),
            },
            Err(error) => Err(turn_store_error(error)),
        }
    }

    fn confirm_unchanged_generation(
        &self,
        claim: &TurnClaim,
        previous: &AgentCompositionPin,
    ) -> TurnResult<bool> {
        let mut state = lock_state(&self.inner);
        settled(self.validate_claim(&state, claim)?)?;
        let session = state
            .sessions
            .get_mut(claim.session_id())
            .expect("validated Session");
        if !session.composition.same_generation(previous) {
            return Ok(false);
        }
        let cleared = session.generation_rejection.take().is_some();
        session.generation_attempt = None;
        drop(state);
        if cleared {
            self.inner.session_changes.committed(claim.session_id());
        }
        Ok(true)
    }

    #[expect(
        clippy::too_many_lines,
        reason = "Keep predecessor capture, detached validation and publication preparation in one ordered adoption flow."
    )]
    pub(super) async fn prepare_generation(
        &self,
        claim: &TurnClaim,
        cancellation: &CancellationToken,
    ) -> TurnResult<Option<Box<dyn PreparedCompositionUpdate>>> {
        // Program children retain the run's exact executable selection. Validate
        // the claim without consuming a submission slot for this read-only path.
        if let Some(ExecutionOwner::ProgramRun { .. }) = claim.header().execution_owner() {
            let state = lock_state(&self.inner);
            settled(self.validate_claim(&state, claim)?)?;
            return Ok(None);
        }
        let (admission, previous, candidate, live, output) = loop {
            let previous = {
                let state = lock_state(&self.inner);
                settled(self.validate_claim(&state, claim)?)?;
                if state.sessions[claim.session_id()]
                    .generation_deferred_turn
                    .as_ref()
                    == Some(claim.turn_id())
                {
                    return Ok(None);
                }
                state.sessions[claim.session_id()].composition.clone()
            };
            let selected = claim
                .header()
                .for_preset(previous.preset_id())
                .map_err(|error| TurnError::Invalid(error.to_string()))?;
            let candidate = match self
                .inner
                .composition
                .prepare_update(&selected, &previous)
                .await
            {
                Ok(Some(candidate)) => candidate,
                Ok(None) if self.confirm_unchanged_generation(claim, &previous)? => {
                    return Ok(None);
                }
                Ok(None) => continue,
                Err(error) => {
                    let error = candidate_error(error);
                    if matches!(error, TurnError::Composition(_)) {
                        let mut state = lock_state(&self.inner);
                        if self.validate_claim(&state, claim).is_ok() {
                            state
                                .sessions
                                .get_mut(claim.session_id())
                                .expect("validated Session")
                                .generation_deferred_turn = Some(claim.turn_id().clone());
                        }
                    }
                    return Err(error);
                }
            };
            let live = {
                let state = lock_state(&self.inner);
                settled(self.validate_claim(&state, claim)?)?;
                let session = &state.sessions[claim.session_id()];
                if session.generation_attempt.as_ref().is_some_and(|attempt| {
                    attempt.rejected
                        && attempt.context_sensitive
                        && attempt.turn_id == *claim.turn_id()
                        && &attempt.selection == candidate.selection()
                }) {
                    return Ok(None);
                }
                session.live_seq().map_err(turn_kernel_error)?
            };
            self.flush(claim, live).await?;
            let admission = self
                .inner
                .submission_admission
                .acquire(claim.session_id())
                .await?;
            let output = {
                let state = lock_state(&self.inner);
                let turn = self.validate_claim(&state, claim)?;
                settled(turn)?;
                let session = &state.sessions[claim.session_id()];
                if !session.composition.same_generation(&previous)
                    || session.live_seq().map_err(turn_kernel_error)? != live
                {
                    continue;
                }
                structured::contract(claim.header(), turn).cloned()
            };
            break (admission, previous, candidate, live, output);
        };
        let cuts = self
            .inner
            .store
            .read_domain_watermarks(claim.session_id())
            .await
            .map_err(turn_store_error)?;
        {
            let mut state = lock_state(&self.inner);
            let session = state
                .sessions
                .get_mut(claim.session_id())
                .ok_or(TurnError::StaleClaim)?;
            if session.generation_attempt.as_ref().is_some_and(|attempt| {
                attempt.rejected
                    && &attempt.selection == candidate.selection()
                    && (attempt.context_sensitive || attempt.domain_seq == cuts.domain_seq)
                    && &attempt.turn_id == claim.turn_id()
            }) {
                return Ok(None);
            }
            session.generation_attempt = Some(Attempt {
                selection: candidate.selection().clone(),
                domain_seq: cuts.domain_seq,
                context_sensitive: false,
                turn_id: claim.turn_id().clone(),
                rejected: false,
            });
        }
        let page = observation::read_domain_states_bounded(&self.inner, claim.session_id(), None)
            .await
            .map_err(turn_store_error)?;
        drop(admission);
        let seed = AgentGenerationSeed::new(
            page.states
                .iter()
                .map(|state| state.snapshot.clone())
                .collect(),
        )
        .map_err(turn_composition_error)?;
        let composition = candidate
            .build(&seed)
            .await
            .map_err(candidate_error)?
            .for_session_preset(claim.header().agent_preset_id());
        let seq = page
            .durable_control_seq
            .checked_add(1)
            .ok_or_else(|| TurnError::Invariant("control sequence exhausted".into()))?;
        let adoption = adoption_states(&composition, &page.states, seq)?;
        validate_private_state(claim.header(), &composition, &adoption.states, cancellation)
            .await?;
        let claim_composition = narrow(claim, composition.clone(), output)?;
        let commit = DomainStateCommit::new(
            None,
            DomainMutationSource::Generation {
                selection: composition.selection().clone(),
                turn_id: Some(claim.turn_id().clone()),
            },
            adoption.updates,
        )
        .and_then(|commit| commit.with_removed(adoption.removed))
        .map_err(|error| TurnError::Composition(error.to_string()))?;
        let control = AgentControlRecord::new(
            seq,
            self.inner.clock.now_ms().max(1),
            AgentControlRecordBody::DomainStateCommitted { commit },
        )
        .map_err(|error| TurnError::Composition(error.to_string()))?;
        let caller = self.agent_caller(claim)?;
        let lease = self.admit_agent_mutation(&caller, cancellation)?;
        if let Some(attempt) = lock_state(&self.inner)
            .sessions
            .get_mut(claim.session_id())
            .and_then(|session| session.generation_attempt.as_mut())
        {
            attempt.context_sensitive = true;
        }
        Ok(Some(Box::new(PreparedGeneration {
            kernel: self.clone(),
            caller,
            lease,
            previous,
            composition,
            claim_composition,
            append: AtomicSessionAppend {
                session_id: claim.session_id().clone(),
                expected_fact_seq: live,
                expected_control_seq: page.durable_control_seq,
                header: None,
                facts: vec![],
                controls: vec![control],
            },
        })))
    }
}

fn candidate_error(error: AgentCompositionError) -> TurnError {
    match error {
        AgentCompositionError::ShuttingDown => TurnError::ShuttingDown,
        AgentCompositionError::Capacity => TurnError::Capacity,
        other => TurnError::Composition(bounded_diagnostic(&other.to_string())),
    }
}

pub(super) async fn validate_cold_state(
    header: &SessionHeader,
    composition: &AgentCompositionPin,
    states: &[rsi_agent_session_protocol::DomainStateView],
) -> TurnResult<()> {
    let cancellation = CancellationToken::new();
    let _cancel = cancellation.clone().drop_guard();
    tokio::time::timeout(
        PREPARATION_BUDGET,
        validate_private_state(header, composition, states, &cancellation),
    )
    .await
    .map_err(|_| TurnError::Composition("current plugin validation exceeded 30 seconds".into()))?
}

pub(super) async fn validate_private_state(
    header: &SessionHeader,
    composition: &AgentCompositionPin,
    states: &[rsi_agent_session_protocol::DomainStateView],
    cancellation: &CancellationToken,
) -> TurnResult<()> {
    for entry in composition.contributions().entries() {
        if let rsi_agent_composition_protocol::ContributionKind::GenerationValidation(validator) =
            entry.kind()
        {
            tokio::select! {
                biased;
                () = cancellation.cancelled() => return Err(TurnError::Cancelled),
                result = validator.validate(header, states, cancellation.clone()) => {
                    result.map_err(|error| match error {
                        rsi_agent_composition_protocol::ContributionError::Closed
                        | rsi_agent_composition_protocol::ContributionError::RegistrationUnavailable => TurnError::ExecutionUnavailable,
                        rsi_agent_composition_protocol::ContributionError::Capacity => TurnError::Capacity,
                        other => TurnError::Composition(bounded_diagnostic(&other.to_string())),
                    })?;
                }
            }
        }
    }
    Ok(())
}

fn settled(turn: &TurnControl) -> TurnResult<()> {
    if turn.terminal.is_some() || turn.conclusion.is_some() || turn.cancellation.is_cancelled() {
        return Err(TurnError::Cancelled);
    }
    turn_state::ensure_no_active_effect(turn)?;
    if turn
        .tool_source
        .as_ref()
        .is_some_and(|source| source.pending_source().is_some())
    {
        return Err(TurnError::Invalid(
            "generation replacement precedes outstanding Tool call settlement".into(),
        ));
    }
    Ok(())
}

pub(super) struct Adoption {
    pub(super) states: Vec<rsi_agent_session_protocol::DomainStateView>,
    pub(super) updates: Vec<DomainStateUpdate>,
    pub(super) removed: Vec<rsi_agent_session_protocol::DomainIdentity>,
}

pub(super) fn adoption_states(
    composition: &AgentCompositionPin,
    current: &[rsi_agent_store_protocol::StoreDomainState],
    control_seq: u64,
) -> TurnResult<Adoption> {
    let outputs = ToolOutputCatalog::identity();
    let baseline = composition.domains().baseline();
    let mut adoption = Adoption {
        states: Vec::with_capacity(baseline.len()),
        updates: Vec::new(),
        removed: current
            .iter()
            .filter(|state| {
                !baseline
                    .iter()
                    .any(|initial| initial.identity().id() == state.snapshot.identity().id())
            })
            .map(|state| state.snapshot.identity().clone())
            .collect(),
    };
    for initial in baseline {
        let existing = current
            .iter()
            .find(|state| state.snapshot.identity().id() == initial.identity().id());
        if let Some(state) = existing {
            composition
                .domains()
                .validate_snapshot(&state.snapshot)
                .map_err(|error| TurnError::Composition(error.to_string()))?;
        }
        let snapshot = match existing {
            Some(state) if initial.identity() != &outputs => state.snapshot.clone(),
            _ => initial.clone(),
        };
        let revision = match existing {
            Some(state) if state.snapshot == snapshot => state.head.revision,
            existing => {
                let update = match existing {
                    Some(state) => DomainStateUpdate::new(state.head.revision, snapshot.clone()),
                    None => DomainStateUpdate::introduced(control_seq, snapshot.clone()),
                }
                .map_err(|error| TurnError::Composition(error.to_string()))?;
                let revision = update.revision();
                adoption.updates.push(update);
                revision
            }
        };
        adoption
            .states
            .push(rsi_agent_session_protocol::DomainStateView { revision, snapshot });
    }
    Ok(adoption)
}

#[cfg(test)]
mod tests {
    #[test]
    fn composition_capacity_is_not_a_memoized_candidate_rejection() {
        assert!(matches!(
            super::candidate_error(super::AgentCompositionError::Capacity),
            super::TurnError::Capacity
        ));
    }
}
