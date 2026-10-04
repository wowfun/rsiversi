use super::*;
use rsi_agent_turn_protocol::{
    ProgramCancelReceipt, ProgramChildView, ProgramDetails, ProgramHistoryPage, ProgramOverview,
};
impl AgentKernel {
    fn program_owner(&self, session: &SessionId, run: &ProgramRunId) -> Option<Arc<LiveRun>> {
        self.inner
            .programs
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(session)
            .and_then(Weak::upgrade)
            .filter(|owner| owner.run_id() == run)
    }
    fn overview(&self, state: &RunState, accepted: u64) -> ProgramOverview {
        ProgramOverview {
            session_id: state.descriptor.session_id.clone(),
            run_id: state.descriptor.run_id.clone(),
            accepted_control_seq: accepted,
            control_seq: state.control_seq,
            started: state.started,
            detached: state.detached,
            cancelling: state.cancelling,
            orphaned: state.outcome.is_none()
                && self
                    .program_owner(&state.descriptor.session_id, &state.descriptor.run_id)
                    .is_none(),
            children: state.children.len(),
            settled_children: state
                .children
                .values()
                .filter(|c| c.receipt.is_some())
                .count(),
            phase: state.phase.clone(),
            progress: state.progress.clone(),
            outcome: state.outcome.clone(),
            script_ref: state.descriptor.script.clone(),
            result_ref: state.result.clone(),
            retention: None,
        }
    }
    pub(crate) async fn session_programs(
        &self,
        session: &SessionId,
        seed: u64,
        before: Option<u64>,
        limit: usize,
    ) -> TurnResult<ProgramHistoryPage> {
        if limit == 0 || limit > rsi_agent_store_protocol::MAXIMUM_PROGRAM_HISTORY_ROWS {
            return Err(invalid("workflow list limit must be 1..=16"));
        }
        let page = self
            .inner
            .store
            .list_program_history(session, seed, before, limit)
            .await
            .map_err(turn_store_error)?;
        page.validate(seed, before, limit)
            .map_err(turn_store_error)?;
        let mut runs = Vec::with_capacity(page.runs.len());
        for head in page.runs {
            let mut reservation = self
                .inner
                .program_bytes
                .reserve(rsi_agent_turn_protocol::MAXIMUM_PROGRAM_OVERVIEW_BYTES)
                .map_err(|_| TurnError::Capacity)?;
            let state = self.read_program_state(session, &head.run_id).await?;
            let accepted = state.accepted_control_seq;
            if accepted != head.first_control_seq
                || accepted > seed
                || before.is_some_and(|b| accepted >= b)
            {
                return Err(invalid(
                    "workflow acceptance index disagrees with canonical state",
                ));
            }
            let mut overview = self.overview(&state, accepted);
            let weight = rsi_api_protocol::measure_json(&overview, reservation.bytes())
                .map_err(|error| invalid(&error.to_string()))?;
            reservation
                .shrink(weight)
                .map_err(|error| invalid(&error.to_string()))?;
            overview.retention = Some(Arc::new(reservation));
            runs.push(overview);
        }
        Ok(ProgramHistoryPage {
            runs,
            has_more: page.has_more,
        })
    }
    pub(crate) async fn session_program(
        &self,
        session: &SessionId,
        run: &ProgramRunId,
        request: rsi_agent_turn_protocol::ProgramRead,
    ) -> TurnResult<ProgramDetails> {
        let page_size = rsi_agent_turn_protocol::PROGRAM_CHILD_PAGE_SIZE;
        if request.children_offset > rsi_agent_session_protocol::MAXIMUM_PROGRAM_CHILDREN as usize
            || !request.children_offset.is_multiple_of(page_size)
        {
            return Err(invalid("invalid workflow child cursor"));
        }
        let mut reservation = self
            .inner
            .program_bytes
            .reserve(rsi_agent_turn_protocol::MAXIMUM_PROGRAM_DETAILS_BYTES)
            .map_err(|_| TurnError::Capacity)?;
        let state = self.read_program_state(session, run).await?;
        if request
            .expected_control_seq
            .is_some_and(|revision| revision != state.control_seq)
        {
            return Err(invalid("workflow revision changed; refresh details"));
        }
        if request.children_offset > state.children.len() {
            return Err(invalid("child cursor exceeds run"));
        }
        let end = (request.children_offset + page_size).min(state.children.len());
        let children = state
            .children
            .iter()
            .skip(request.children_offset)
            .take(page_size)
            .map(|(ordinal, child)| ProgramChildView {
                ordinal: *ordinal,
                session_id: child.session_id.clone(),
                receipt: child.receipt.clone(),
            })
            .collect();
        let mut details = ProgramDetails {
            overview: self.overview(&state, state.accepted_control_seq),
            children,
            children_offset: request.children_offset,
            next_children_offset: (end < state.children.len()).then_some(end),
            result: None,
            script: None,
        };
        let weight = rsi_api_protocol::measure_json(&details, reservation.bytes())
            .map_err(|error| invalid(&error.to_string()))?;
        reservation
            .shrink(weight)
            .map_err(|error| invalid(&error.to_string()))?;
        details.overview.retention = Some(Arc::new(reservation));
        // The validated immutable state owns both references across CAS I/O.
        if request.result {
            let binding = state
                .result
                .as_ref()
                .ok_or_else(|| invalid("workflow has no result"))?;
            details.result = Some(self.read_program_blob(binding).await?);
        }
        if request.script {
            details.script = Some(self.read_program_blob(&state.descriptor.script).await?);
        }
        Ok(details)
    }
    pub(super) async fn read_program_blob(
        &self,
        binding: &rsi_agent_session_protocol::ProgramBlob,
    ) -> TurnResult<rsi_api_protocol::RetainedBytes> {
        self.inner
            .store
            .read_cas(
                &rsi_agent_store_protocol::CasObjectRef {
                    sha256: binding.sha256.clone(),
                    byte_len: binding.bytes,
                },
                self.inner.program_bytes.clone().into(),
            )
            .await
            .map_err(|error| match error {
                StoreError::NotFound(digest) => {
                    TurnError::Store(format!("missing workflow CAS object {digest}"))
                }
                error => turn_store_error(error),
            })
    }
    pub(crate) async fn cancel_session_run(
        &self,
        session: &SessionId,
        run: &ProgramRunId,
    ) -> TurnResult<ProgramCancelReceipt> {
        let state = self.read_program_state(session, run).await?;
        if let Some(outcome) = &state.outcome {
            return Ok(ProgramCancelReceipt::AlreadyTerminal {
                run_id: run.clone(),
                control_seq: state.control_seq,
                outcome: outcome.clone(),
            });
        }
        let Some(owner) = self.program_owner(session, run) else {
            let state = self.read_program_state(session, run).await?;
            if let Some(outcome) = &state.outcome {
                return Ok(ProgramCancelReceipt::AlreadyTerminal {
                    run_id: run.clone(),
                    control_seq: state.control_seq,
                    outcome: outcome.clone(),
                });
            }
            return Ok(ProgramCancelReceipt::OrphanedRequiresRestart {
                run_id: run.clone(),
                control_seq: state.control_seq,
                cancellation_requested: state.cancelling,
            });
        };
        let kernel = self.clone();
        let session = session.clone();
        let run = run.clone();
        self.owned_commit(async move {
            owner.request_cancellation().await?;
            let state = kernel
                .read_program_state(&session, &run)
                .await
                .map_err(|_| TurnError::ExecutionOutcomeUnknown)?;
            match &state.outcome {
                Some(outcome) => Ok(ProgramCancelReceipt::AlreadyTerminal {
                    run_id: run.clone(),
                    control_seq: state.control_seq,
                    outcome: outcome.clone(),
                }),
                None if state.cancelling => Ok(ProgramCancelReceipt::Accepted {
                    run_id: run.clone(),
                    control_seq: state.control_seq,
                }),
                None => Err(TurnError::ExecutionOutcomeUnknown),
            }
        })
        .await
        .map_err(|error| match error {
            TurnError::Invariant(_) => TurnError::ExecutionOutcomeUnknown,
            error => error,
        })
    }
}
