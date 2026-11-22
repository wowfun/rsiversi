//! Session-owned background settlements and current-claim status reads.
use super::*;
use rsi_agent_turn_protocol::{
    MAXIMUM_TURN_JOBS_BYTES, TurnJobStatusSource, TurnJobs, TurnJobsPage, TurnJobsRequest,
};

#[derive(Debug)]
struct SessionJobOwner {
    kernel: Weak<KernelInner>,
    session: SessionId,
    header: String,
    execution: Option<rsi_execution::ExecutionLease>,
}

impl AgentKernel {
    pub(super) fn background_jobs(
        &self,
        claim: &TurnClaim,
        header: &str,
    ) -> TurnResult<rsi_jobs::BackgroundJobs> {
        let scope = rsi_agent_turn_protocol::session_job_scope(header)
            .map_err(|error| TurnError::Invalid(error.to_string()))?;
        Ok(rsi_jobs::BackgroundJobs {
            scope,
            completion: Arc::new(SessionJobOwner {
                kernel: Arc::downgrade(&self.inner),
                session: claim.session_id().clone(),
                header: header.into(),
                execution: claim.execution().cloned(),
            }),
        })
    }
}

impl SessionJobOwner {
    async fn deliver(
        &self,
        kernel: &AgentKernel,
        job: rsi_jobs::JobSummary,
        output_bytes: u64,
        jobs: &dyn rsi_jobs::Jobs,
        scope: &rsi_jobs::JobScopeAuthority,
    ) -> TurnResult<()> {
        let admission = kernel
            .inner
            .submission_admission
            .acquire(&self.session)
            .await?;
        let mut prepared = kernel.prepare_resume(&self.session).await?;
        if prepared
            .header()
            .fingerprint()
            .map_err(|error| TurnError::Invalid(error.to_string()))?
            != self.header
        {
            return Err(TurnError::StaleClaim);
        }
        if let Some(execution) = &self.execution {
            prepared = prepared.with_execution(execution.clone())?;
        }
        let origin = job.origin.as_deref().ok_or_else(|| {
            TurnError::Invariant("background job lacks its originating effect".into())
        })?;
        let id = format!("job-notice-{:x}", Sha256::digest(origin.as_bytes()));
        let status = match job.status {
            rsi_jobs::JobStatus::Completed => "completed",
            rsi_jobs::JobStatus::Failed => "failed",
            rsi_jobs::JobStatus::Cancelled => "cancelled",
            rsi_jobs::JobStatus::OutcomeUnknown => "outcome unknown",
            _ => {
                return Err(TurnError::Invariant(
                    "background completion requires a terminal status".into(),
                ));
            }
        };
        let summary = format!("Background job {} ({}) {status}", job.id, job.name);
        let text = if output_bytes == 0 {
            summary.clone()
        } else {
            let preview = if output_bytes <= 2048 {
                jobs.peek(scope, &job.id, origin, [2048; 2])
                    .map_err(|error| TurnError::Invalid(error.to_string()))?
            } else {
                None
            };
            completion_text(&summary, preview)
        };
        let message = AgentMessage {
            message_id: MessageId::new(id.clone())
                .map_err(|error| TurnError::Invalid(error.to_string()))?,
            source: AgentMessageSource::Application {
                attribution: Some(rsi_agent_session_protocol::MessageAttribution {
                    id,
                    source: serde_json::json!({"kind":"tool-jobs","form":"notice","summary":summary}),
                }),
            },
            content: vec![AgentMessageContent::Text { text }],
            options: MessageOptions::default(),
        };
        kernel
            .submit_message_admitted(
                SubmitMessage {
                    session: SubmitSession::Resume(prepared),
                    message,
                    delivery: rsi_agent_session_protocol::MessageDelivery::Steer,
                },
                crate::turn_service::MessageAdmission::BackgroundJob,
                admission,
            )
            .await?;
        Ok(())
    }
}

fn completion_text(summary: &str, preview: Option<rsi_jobs::JobRead>) -> String {
    if let Some(preview) = preview
        && !preview.stdout.lossy
        && !preview.stderr.lossy
        && preview.stdout.oldest_offset == 0
        && preview.stderr.oldest_offset == 0
        && preview.stdout.bytes.len() + preview.stderr.bytes.len() <= 2048
    {
        let mut text = summary.to_owned();
        for (label, bytes) in [
            ("stdout", preview.stdout.bytes),
            ("stderr", preview.stderr.bytes),
        ] {
            if !bytes.is_empty() {
                text.push_str(&format!("\n{label}\n"));
                text.extend(String::from_utf8_lossy(&bytes).chars().map(|c| {
                    if c.is_control() && !matches!(c, '\n' | '\t') {
                        '�'
                    } else {
                        c
                    }
                }));
            }
        }
        if text.len() <= 2048 {
            return text;
        }
    }
    format!("{summary}. Read retained output with job_output.")
}

#[async_trait]
impl rsi_jobs::JobCompletion for SessionJobOwner {
    async fn settled(
        &self,
        job: rsi_jobs::JobSummary,
        output_bytes: u64,
        jobs: &dyn rsi_jobs::Jobs,
        scope: &rsi_jobs::JobScopeAuthority,
    ) -> bool {
        let Some(inner) = self.kernel.upgrade() else {
            return false;
        };
        let kernel = AgentKernel { inner };
        match self.deliver(&kernel, job, output_bytes, jobs, scope).await {
            Ok(()) => true,
            Err(error) => {
                kernel.record_ready_failure(&error);
                false
            }
        }
    }
}

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
        if session.header_fingerprint != header {
            return Err(TurnError::StaleClaim);
        }
        session
            .turns
            .get(turn_id)
            .and_then(|turn| live_job_source(&state, turn))
            .ok_or(TurnError::StaleClaim)
    }
}

fn live_job_source(
    state: &KernelState,
    turn: &TurnControl,
) -> Option<(u64, Arc<dyn TurnJobStatusSource>)> {
    let owner = turn.claim.as_ref()?;
    if turn.terminal.is_some()
        || owner.mutations.is_retiring()
        || state.executors.get(&owner.executor) != Some(&owner.registration)
    {
        return None;
    }
    Some((owner.claim, owner.jobs.as_ref()?.upgrade()?))
}

#[async_trait]
impl TurnJobs for AgentKernel {
    fn active_jobs(&self, session: &SessionId) -> TurnResult<Vec<rsi_jobs::ActiveJob>> {
        let source = {
            let state = lock_state(&self.inner);
            if !state.accepting {
                return Err(TurnError::ShuttingDown);
            }
            state.sessions.get(session).and_then(|runtime| {
                let turn = runtime.turns.get(runtime.oldest_claimable()?)?;
                live_job_source(&state, turn)
            })
        };
        let Some((_, source)) = source else {
            return Ok(Vec::new());
        };
        match source.active() {
            Err(TurnError::StaleClaim) => Ok(Vec::new()),
            result => result,
        }
    }

    async fn peek_job(
        &self,
        session: &SessionId,
        header_sha256: &str,
        request: rsi_agent_turn_protocol::JobPreviewRequest,
        cancellation: CancellationToken,
    ) -> TurnResult<rsi_agent_turn_protocol::JobPreviewPage> {
        use rsi_agent_turn_protocol::{JobPreview, JobPreviewPage, JobPreviewStream};
        request.validate()?;
        if cancellation.is_cancelled() {
            return Err(TurnError::ShuttingDown);
        }
        let (generation, source) = self.job_source(session, header_sha256, &request.turn_id)?;
        if generation != request.generation || !source.is_active() {
            return Err(TurnError::StaleClaim);
        }
        let preview = source
            .peek(&request)?
            .map(|read| {
                if read.job.id != request.job_id
                    || read.job.origin.as_deref() != Some(request.effect_id.as_str())
                {
                    return Err(TurnError::Invalid("job preview origin differs".into()));
                }
                read.job
                    .validate()
                    .map_err(|error| TurnError::Invalid(error.to_string()))?;
                Ok(JobPreview {
                    status: read.job.status,
                    stdout: JobPreviewStream::from_read(read.stdout, request.stdout_bytes)?,
                    stderr: JobPreviewStream::from_read(read.stderr, request.stderr_bytes)?,
                })
            })
            .transpose()?;
        let page = JobPreviewPage {
            session_id: session.clone(),
            header_sha256: header_sha256.into(),
            request: request.clone(),
            preview,
        };
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
