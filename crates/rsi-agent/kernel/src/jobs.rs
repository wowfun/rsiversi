//! Weak current-claim status relay, with no Jobs authority construction.
use super::*;
use rsi_agent_turn_protocol::{
    MAXIMUM_TURN_JOBS_BYTES, TurnJobStatusSource, TurnJobs, TurnJobsPage, TurnJobsRequest,
};

impl AgentKernel {
    fn job_source(
        &self,
        session_id: &SessionId,
        header: &str,
        turn_id: &TurnId,
    ) -> TurnResult<(u64, Arc<dyn TurnJobStatusSource>)> {
        let state = lock_state(&self.inner);
        if !state.accepting {
            return Err(TurnError::ShuttingDown);
        }
        let session = state
            .sessions
            .get(session_id)
            .ok_or(TurnError::StaleClaim)?;
        if session
            .header
            .fingerprint()
            .map_err(|error| TurnError::Invalid(error.to_string()))?
            != header
        {
            return Err(TurnError::StaleClaim);
        }
        let turn = session
            .turns
            .get(turn_id)
            .filter(|turn| turn.terminal.is_none())
            .ok_or(TurnError::StaleClaim)?;
        let owner = turn
            .claim
            .as_ref()
            .filter(|owner| {
                !owner.mutations.is_retiring()
                    && state.executors.get(&owner.executor) == Some(&owner.registration)
            })
            .ok_or(TurnError::StaleClaim)?;
        let source = owner
            .jobs
            .as_ref()
            .and_then(Weak::upgrade)
            .ok_or(TurnError::StaleClaim)?;
        Ok((owner.claim, source))
    }
}

#[async_trait]
impl TurnJobs for AgentKernel {
    async fn read_jobs(
        &self,
        session: &SessionId,
        header_sha256: &str,
        request: TurnJobsRequest,
        cancellation: CancellationToken,
    ) -> TurnResult<TurnJobsPage> {
        request.validate()?;
        if cancellation.is_cancelled() {
            return Err(TurnError::ShuttingDown);
        }
        let (generation, source) = self.job_source(session, header_sha256, &request.turn_id)?;
        if !source.is_active()
            || request
                .generation
                .is_some_and(|previous| previous != generation)
        {
            return Err(TurnError::StaleClaim);
        }
        let mut jobs = source.list()?;
        if jobs.len() > rsi_jobs::MAXIMUM_JOBS_PER_LIST {
            return Err(TurnError::Invalid("Jobs source exceeded row bound".into()));
        }
        for job in &jobs {
            job.validate()
                .map_err(|error| TurnError::Invalid(error.to_string()))?;
        }
        jobs.sort_unstable_by(|left, right| left.id.cmp(&right.id));
        if jobs.windows(2).any(|pair| pair[0].id == pair[1].id) {
            return Err(TurnError::Invalid(
                "Jobs source repeated an identity".into(),
            ));
        }
        jobs.retain(|job| request.after.as_ref().is_none_or(|after| &job.id > after));
        let has_more = jobs.len() > request.limit;
        jobs.truncate(request.limit);
        let mut page = TurnJobsPage {
            session_id: session.clone(),
            header_sha256: header_sha256.into(),
            turn_id: request.turn_id.clone(),
            generation,
            after: request.after.clone(),
            jobs,
            has_more,
        };
        while page.encoded_len()? > MAXIMUM_TURN_JOBS_BYTES && !page.jobs.is_empty() {
            page.jobs.pop();
            page.has_more = true;
        }
        page.validate_for(session, header_sha256, &request)?;
        let (current_generation, current_source) =
            self.job_source(session, header_sha256, &request.turn_id)?;
        if cancellation.is_cancelled() {
            return Err(TurnError::ShuttingDown);
        }
        if current_generation != generation
            || !Arc::ptr_eq(&source, &current_source)
            || !source.is_active()
        {
            return Err(TurnError::StaleClaim);
        }
        Ok(page)
    }
}
