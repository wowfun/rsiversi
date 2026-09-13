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
