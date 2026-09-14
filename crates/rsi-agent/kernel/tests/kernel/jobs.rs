use super::*;
use rsi_agent_turn_protocol::{TurnJobStatusSource, TurnJobs, TurnJobsRequest};
use rsi_jobs::{JobStatus, JobSummary, JobTerminal};

#[derive(Debug)]
struct Source {
    rows: Vec<JobSummary>,
    active: AtomicBool,
    revoke_during_read: AtomicBool,
    reads: AtomicUsize,
}
impl Source {
    fn new(count: usize) -> Arc<Self> {
        Arc::new(Self {
            rows: (0..count)
                .map(|i| JobSummary {
                    id: format!("job-{i:03}"),
                    name: "background".into(),
                    producer: "fixture".into(),
                    origin: None,
                    status: JobStatus::Completed,
                    requires_report: true,
                    reported: false,
                    terminal: Some(JobTerminal {
                        status: JobStatus::Completed,
                        exit_code: Some(0),
                        signal: None,
                        message: Some("\u{1}".repeat(4096)),
                    }),
                    output_retained: true,
                })
                .collect(),
            active: AtomicBool::new(true),
            revoke_during_read: AtomicBool::new(false),
            reads: AtomicUsize::new(0),
        })
    }
}
impl TurnJobStatusSource for Source {
    fn peek(
        &self,
        _: &rsi_agent_turn_protocol::JobPreviewRequest,
    ) -> rsi_agent_turn_protocol::Result<Option<rsi_jobs::JobRead>> {
        self.reads.fetch_add(1, Ordering::SeqCst);
        if self.revoke_during_read.load(Ordering::SeqCst) {
            self.active.store(false, Ordering::SeqCst);
        }
        Ok(self.rows.first().map(|row| {
            let mut job = row.clone();
            job.origin = Some("effect".into());
            let stream = || rsi_jobs::JobOutputRead {
                bytes: vec![0, 255, 10],
                oldest_offset: 7,
                next_offset: 10,
                lossy: true,
                full_output: None,
            };
            rsi_jobs::JobRead {
                job,
                stdout: stream(),
                stderr: stream(),
            }
        }))
    }
    fn is_active(&self) -> bool {
        self.active.load(Ordering::SeqCst)
    }
    fn list(&self) -> rsi_agent_turn_protocol::Result<Vec<JobSummary>> {
        self.reads.fetch_add(1, Ordering::SeqCst);
        if self.revoke_during_read.load(Ordering::SeqCst) {
            self.active.store(false, Ordering::SeqCst);
        }
        Ok(self.rows.clone())
    }
}

#[tokio::test]
#[allow(clippy::too_many_lines)] // One scenario follows a cursor through byte truncation, retirement and replacement.
async fn jobs_pages_are_byte_bounded_and_claim_scoped_without_reporting() {
    let kernel = kernel(Arc::new(MemoryStore::new())).await;
    let worker = kernel.start_workers();
    let submitted = submit(&kernel, "jobs-paging", "work").await;
    let registration = kernel.register("executor".into()).unwrap();
    let claim = kernel
        .claim("executor", CancellationToken::new())
        .await
        .unwrap()
        .unwrap();
    let source = Source::new(9);
    let erased: Arc<dyn TurnJobStatusSource> = source.clone();
    kernel
        .publish_job_status(&claim, Arc::downgrade(&erased))
        .unwrap();
    assert!(
        kernel
            .publish_job_status(&claim, Arc::downgrade(&erased))
            .is_err()
    );
    let header = claim.header().fingerprint().unwrap();
    let mut request = TurnJobsRequest {
        turn_id: submitted.turn_id,
        generation: None,
        after: None,
        limit: 32,
    };
    let mut seen = Vec::new();
    loop {
        let page = kernel
            .read_jobs(
                &submitted.session_id,
                &header,
                request.clone(),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        page.validate_for(&submitted.session_id, &header, &request)
            .unwrap();
        assert!(page.encoded_len().unwrap() <= 64 * 1024);
        assert!(
            page.jobs.len() <= 2,
            "escaped diagnostics must count toward bytes"
        );
        assert!(
            page.jobs
                .iter()
                .all(|job| !job.reported && job.output_retained)
        );
        seen.extend(page.jobs.iter().map(|job| job.id.clone()));
        request.generation = Some(page.generation);
        request.after = page.jobs.last().map(|job| job.id.clone());
        if !page.has_more {
            break;
        }
    }
    assert_eq!(
        seen,
        source
            .rows
            .iter()
            .map(|job| job.id.clone())
            .collect::<Vec<_>>()
    );
    assert!(matches!(
        kernel
            .read_jobs(
                &submitted.session_id,
                &"b".repeat(64),
                request.clone(),
                CancellationToken::new()
            )
            .await,
        Err(TurnError::StaleClaim)
    ));
    kernel.release(&claim).unwrap();
    assert!(matches!(
        kernel
            .read_jobs(
                &submitted.session_id,
                &header,
                request.clone(),
                CancellationToken::new()
            )
            .await,
        Err(TurnError::StaleClaim)
    ));
    let replacement = kernel
        .claim("executor", CancellationToken::new())
        .await
        .unwrap()
        .unwrap();
    kernel
        .publish_job_status(&replacement, Arc::downgrade(&erased))
        .unwrap();
    assert!(matches!(
        kernel
            .read_jobs(
                &submitted.session_id,
                &header,
                request,
                CancellationToken::new()
            )
            .await,
        Err(TurnError::StaleClaim)
    ));
    drop(registration);
    kernel.shutdown(worker).await.unwrap();
}

#[tokio::test]
async fn jobs_source_revocation_and_cancellation_fence_sampled_results() {
    let kernel = kernel(Arc::new(MemoryStore::new())).await;
    let worker = kernel.start_workers();
    let submitted = submit(&kernel, "jobs-revoke", "work").await;
    let _registration = kernel.register("executor".into()).unwrap();
    let claim = kernel
        .claim("executor", CancellationToken::new())
        .await
        .unwrap()
        .unwrap();
    let source = Source::new(1);
    let erased: Arc<dyn TurnJobStatusSource> = source.clone();
    kernel
        .publish_job_status(&claim, Arc::downgrade(&erased))
        .unwrap();
    let header = claim.header().fingerprint().unwrap();
    let request = TurnJobsRequest {
        turn_id: submitted.turn_id,
        generation: None,
        after: None,
        limit: 1,
    };
    let cancelled = CancellationToken::new();
    cancelled.cancel();
    assert!(
        kernel
            .read_jobs(&submitted.session_id, &header, request.clone(), cancelled)
            .await
            .is_err()
    );
    assert_eq!(source.reads.load(Ordering::SeqCst), 0);
    source.revoke_during_read.store(true, Ordering::SeqCst);
    assert!(matches!(
        kernel
            .read_jobs(
                &submitted.session_id,
                &header,
                request.clone(),
                CancellationToken::new()
            )
            .await,
        Err(TurnError::StaleClaim)
    ));
    assert_eq!(source.reads.load(Ordering::SeqCst), 1);
    assert!(
        kernel
            .read_jobs(
                &submitted.session_id,
                &header,
                request,
                CancellationToken::new()
            )
            .await
            .is_err()
    );
    assert_eq!(source.reads.load(Ordering::SeqCst), 1);
    kernel.shutdown(worker).await.unwrap();
}

#[tokio::test]
#[allow(clippy::too_many_lines)] // One preview authority is tested before and after every invalidating transition.
async fn job_preview_rejects_foreign_origins_generations_cancellation_and_mid_read_revocation() {
    use rsi_agent_turn_protocol::JobPreviewRequest;
    let kernel = kernel(Arc::new(MemoryStore::new())).await;
    let worker = kernel.start_workers();
    let submitted = submit(&kernel, "peek-session", "work").await;
    let _registration = kernel.register("executor".into()).unwrap();
    let claim = kernel
        .claim("executor", CancellationToken::new())
        .await
        .unwrap()
        .unwrap();
    let source = Source::new(1);
    let erased: Arc<dyn TurnJobStatusSource> = source.clone();
    kernel
        .publish_job_status(&claim, Arc::downgrade(&erased))
        .unwrap();
    let header = claim.header().fingerprint().unwrap();
    let status = kernel
        .read_jobs(
            &submitted.session_id,
            &header,
            TurnJobsRequest {
                turn_id: submitted.turn_id.clone(),
                generation: None,
                after: None,
                limit: 1,
            },
            CancellationToken::new(),
        )
        .await
        .unwrap();
    let request = JobPreviewRequest {
        turn_id: submitted.turn_id.clone(),
        generation: status.generation,
        job_id: "job-000".into(),
        effect_id: EffectId::new("effect").unwrap(),
        stdout_bytes: 32,
        stderr_bytes: 32,
    };
    let page = kernel
        .peek_job(
            &submitted.session_id,
            &header,
            request.clone(),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(
        page.preview.unwrap().stdout.bytes(32).unwrap(),
        vec![0, 255, 10]
    );
    let reads = source.reads.load(Ordering::SeqCst);
    let mut stale = request.clone();
    stale.generation += 1;
    assert!(matches!(
        kernel
            .peek_job(
                &submitted.session_id,
                &header,
                stale,
                CancellationToken::new()
            )
            .await,
        Err(TurnError::StaleClaim)
    ));
    assert!(
        kernel
            .peek_job(
                &SessionId::new("foreign").unwrap(),
                &header,
                request.clone(),
                CancellationToken::new()
            )
            .await
            .is_err()
    );
    assert!(
        kernel
            .peek_job(
                &submitted.session_id,
                &"0".repeat(64),
                request.clone(),
                CancellationToken::new()
            )
            .await
            .is_err()
    );
    let cancelled = CancellationToken::new();
    cancelled.cancel();
    assert!(
        kernel
            .peek_job(&submitted.session_id, &header, request.clone(), cancelled)
            .await
            .is_err()
    );
    assert_eq!(source.reads.load(Ordering::SeqCst), reads);
    let mut wrong = request.clone();
    wrong.effect_id = EffectId::new("wrong-effect").unwrap();
    assert!(
        kernel
            .peek_job(
                &submitted.session_id,
                &header,
                wrong,
                CancellationToken::new()
            )
            .await
            .is_err()
    );
    let mut too_small = request.clone();
    too_small.stdout_bytes = 1;
    assert!(
        kernel
            .peek_job(
                &submitted.session_id,
                &header,
                too_small,
                CancellationToken::new()
            )
            .await
            .is_err()
    );
    source.revoke_during_read.store(true, Ordering::SeqCst);
    assert!(matches!(
        kernel
            .peek_job(
                &submitted.session_id,
                &header,
                request,
                CancellationToken::new()
            )
            .await,
        Err(TurnError::StaleClaim)
    ));
    kernel.shutdown(worker).await.unwrap();
}
