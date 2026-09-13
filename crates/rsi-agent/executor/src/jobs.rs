use super::{Arc, Driver, JobScopeAuthority, Jobs, TurnClaim, TurnError, bounded, failure_outcome};
use rsi_agent_turn_protocol::TurnJobStatusSource;

#[derive(Debug)]
pub(super) struct StatusSource {
    pub(super) jobs: Arc<dyn Jobs>,
    pub(super) scope: JobScopeAuthority,
}
impl TurnJobStatusSource for StatusSource {
    fn is_active(&self) -> bool {
        self.scope.is_active()
    }
    fn list(&self) -> std::result::Result<Vec<rsi_jobs::JobSummary>, TurnError> {
        self.jobs.list(&self.scope).map_err(|error| match error {
            rsi_jobs::JobsError::ScopeClosed => TurnError::StaleClaim,
            rsi_jobs::JobsError::ShuttingDown => TurnError::ShuttingDown,
            other => TurnError::Invalid(bounded(&other.to_string())),
        })
    }
}

impl Driver {
    pub(super) async fn prepare_jobs(
        &self,
        claim: &TurnClaim,
    ) -> Option<(JobScopeAuthority, Arc<dyn TurnJobStatusSource>)> {
        let scope = match self.acquire_job_scope(claim) {
            Ok(scope) => scope,
            Err(message) => {
                let _ignored = self
                    .finish(
                        claim,
                        None,
                        failure_outcome("jobs.scope", bounded(&message)),
                    )
                    .await;
                return None;
            }
        };
        let source: Arc<dyn TurnJobStatusSource> = Arc::new(StatusSource {
            jobs: self.jobs.clone(),
            scope: scope.clone(),
        });
        if let Err(error) = self
            .turns
            .publish_job_status(claim, Arc::downgrade(&source))
        {
            self.finish_context_error(claim, Some(&scope), error.to_string())
                .await;
            return None;
        }
        Some((scope, source))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use rsi_jobs::{
        JobControl, JobOutputRead, JobProducer, JobProducerRegistration, JobRequest, JobScopeId,
        JobStatus, JobStream, JobSubmission, JobTerminal, JobsContract,
    };
    use rsi_meta::{ResolvedFactory, Runtime, UpdateMode};

    #[derive(Debug)]
    struct Completed;
    impl JobProducer for Completed {
        fn start(&self, _: &JobRequest) -> rsi_jobs::Result<Arc<dyn JobControl>> {
            Ok(Arc::new(Self))
        }
    }
    #[async_trait]
    impl JobControl for Completed {
        fn read(&self, _: JobStream, _: u64) -> rsi_jobs::Result<JobOutputRead> {
            Ok(JobOutputRead {
                full_output: None,
                bytes: b"retained".to_vec(),
                oldest_offset: 0,
                next_offset: 8,
                lossy: false,
            })
        }
        fn cancel(&self) {}
        async fn wait(&self) -> rsi_jobs::Result<JobTerminal> {
            Ok(JobTerminal {
                status: JobStatus::Completed,
                exit_code: Some(0),
                signal: None,
                message: None,
            })
        }
    }

    #[tokio::test]
    async fn status_source_never_reports_or_reacquires_after_original_scope_finalization() {
        let runtime = Runtime::default();
        runtime
            .root()
            .apply(
                ResolvedFactory::linked(
                    "jobs",
                    "test",
                    UpdateMode::Replayable,
                    Arc::new(rsi_jobs_local::JobsLocalFactory),
                ),
                serde_json::Value::Null,
            )
            .await
            .unwrap();
        let jobs = runtime.root().lookup_local::<JobsContract>().unwrap();
        let producer = jobs
            .register_producer(JobProducerRegistration {
                name: "fixture".into(),
                producer: Arc::new(Completed),
            })
            .unwrap();
        let identity = JobScopeId::new("rsi.agent.turn", ["session", "turn"]).unwrap();
        let scope = jobs.acquire_scope(identity.clone()).unwrap();
        let id = jobs
            .submit(
                &scope,
                JobSubmission {
                    name: "background".into(),
                    producer: "fixture".into(),
                    request: JobRequest::new(()),
                    requires_report: true,
                },
            )
            .unwrap();
        let source = StatusSource {
            jobs: jobs.clone(),
            scope: scope.clone(),
        };
        let terminal = tokio::time::timeout(std::time::Duration::from_secs(2), async {
            loop {
                let row = source.list().unwrap().pop().unwrap();
                if row.status.is_terminal() {
                    break row;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert!(!terminal.reported);
        assert!(terminal.output_retained);
        assert_eq!(jobs.get(&scope, &id).unwrap(), terminal);
        assert_eq!(
            jobs.finalize_scope(&scope).await.unwrap().unreported.len(),
            1
        );
        assert!(!source.is_active());
        let replacement = jobs.acquire_scope(identity).unwrap();
        assert!(!replacement.same_generation(&scope));
        assert!(jobs.list(&replacement).unwrap().is_empty());
        assert!(matches!(source.list(), Err(TurnError::StaleClaim)));
        producer.retire().await.unwrap();
        assert!(runtime.shutdown().await.is_clean());
    }
}
