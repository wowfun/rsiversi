use super::*;

impl AgentKernel {
    pub(crate) async fn read_program_snapshot(
        &self,
        caller: &AgentCallerAuthority,
        run: &ProgramRunId,
    ) -> TurnResult<rsi_agent_turn_protocol::ProgramSnapshot> {
        self.validate_agent_caller(caller)?;
        let state = self.read_program_state(caller.session_id(), run).await?;
        let phase = state.phase.clone();
        let progress = state.progress.clone();
        let result_ref = state.result.clone();
        let result = if let Some(binding) = &result_ref {
            let bytes = self
                .inner
                .store
                .read_cas(&rsi_agent_store_protocol::CasObjectRef {
                    sha256: binding.sha256.clone(),
                    byte_len: binding.bytes,
                })
                .await
                .map_err(turn_store_error)?;
            if bytes.len() as u64 != binding.bytes
                || format!("{:x}", Sha256::digest(&bytes)) != binding.sha256
            {
                return Err(invalid(
                    "program result bytes differ from their durable binding",
                ));
            }
            Some(serde_json::from_slice(&bytes).map_err(session_error)?)
        } else {
            None
        };
        // Store/CAS awaits may retire the source Tool. Observation must not
        // publish through a caller that lost its authority during those reads.
        self.validate_agent_caller(caller)?;
        Ok(rsi_agent_turn_protocol::ProgramSnapshot {
            run_id: run.clone(),
            control_seq: state.control_seq,
            started: state.started,
            detached: state.detached,
            cancelling: state.cancelling,
            children: state.children.len(),
            settled_children: state
                .children
                .values()
                .filter(|child| child.receipt.is_some())
                .count(),
            phase,
            progress,
            outcome: state.outcome.clone(),
            result_ref,
            result,
        })
    }

    pub(crate) async fn cancel_program_owned(
        &self,
        caller: &AgentCallerAuthority,
        id: &ProgramRunId,
    ) -> TurnResult<()> {
        self.validate_agent_caller(caller)?;
        let source = self.admit_agent_mutation(caller, &CancellationToken::new())?;
        let run = self
            .inner
            .programs
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(caller.session_id())
            .and_then(Weak::upgrade)
            .filter(|run| run.run_id() == id);
        if let Some(run) = run {
            self.owned_commit(async move {
                let _source = source;
                run.cancel().await
            })
            .await
        } else {
            let state = self.read_program_state(caller.session_id(), id).await?;
            if state.outcome.is_some() {
                Ok(())
            } else {
                Err(TurnError::StaleClaim)
            }
        }
    }
}
