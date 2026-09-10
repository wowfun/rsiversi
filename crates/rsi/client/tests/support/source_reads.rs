use super::*;
use rsi_client::SourceReadError;
use rsi_conversation::{FactField, SourceRef};
use std::sync::atomic::{AtomicBool, AtomicU64};
use tokio_util::sync::CancellationToken;

#[derive(Debug)]
pub struct Scenario {
    pub requests: Mutex<Vec<(Option<u64>, usize)>>,
    pub active: Arc<AtomicUsize>,
    pub block: AtomicBool,
    pub seq: AtomicU64,
}
impl Default for Scenario {
    fn default() -> Self {
        Self {
            requests: Mutex::new(Vec::new()),
            active: Arc::default(),
            block: AtomicBool::new(false),
            seq: AtomicU64::new(9),
        }
    }
}
impl Scenario {
    pub async fn read(
        &self,
        before: Option<u64>,
        limit: usize,
    ) -> rsi_session_protocol::Result<rsi_session_protocol::SessionHistoryPage> {
        let _active = Active::new(&self.active);
        self.requests.lock().unwrap().push((before, limit));
        if self.block.load(Ordering::SeqCst) {
            std::future::pending::<()>().await;
        }
        let seq = self.seq.load(Ordering::SeqCst);
        Ok(rsi_session_protocol::SessionHistoryPage {
            before_seq: before.unwrap_or(u64::MAX),
            facts: vec![
                SessionFact::new(
                    seq,
                    1,
                    SessionFactBody::TurnTerminal {
                        turn_id: TurnId::new("turn").unwrap(),
                        outcome: TurnOutcome::Completed,
                    },
                )
                .unwrap(),
            ],
            durable_seq: seq,
            has_more: seq > 1,
        })
    }
}
fn source(seq: u64) -> SourceRef {
    SourceRef {
        seq,
        field: FactField::TurnOutcome,
    }
}

async fn exact_reads(client: &Arc<rsi_client::SessionController>, handle: &Handle) {
    assert!(matches!(
        client
            .source_window(source(9), 0, 3, CancellationToken::new())
            .await,
        Err(SourceReadError::Window(_))
    ));
    assert!(handle.source_reads.requests.lock().unwrap().is_empty());
    let window = client
        .source_window(source(9), 0, 64, CancellationToken::new())
        .await
        .unwrap();
    assert!(window.text.contains("completed") && !window.more);
    assert_eq!(
        *handle.source_reads.requests.lock().unwrap(),
        [(Some(10), 1)]
    );
    assert!(matches!(
        client
            .source_window(source(10), 0, 64, CancellationToken::new())
            .await,
        Err(SourceReadError::Unavailable)
    ));
    assert!(matches!(
        client
            .source_window(
                SourceRef {
                    field: FactField::ToolArguments,
                    ..source(9)
                },
                0,
                64,
                CancellationToken::new()
            )
            .await,
        Err(SourceReadError::Unavailable)
    ));
    let calls = handle.source_reads.requests.lock().unwrap().len();
    let cancelled = CancellationToken::new();
    cancelled.cancel();
    assert!(matches!(
        client.source_window(source(9), 0, 64, cancelled).await,
        Err(SourceReadError::Cancelled)
    ));
    assert_eq!(handle.source_reads.requests.lock().unwrap().len(), calls);
    handle.source_reads.seq.store(u64::MAX, Ordering::SeqCst);
    assert!(
        client
            .source_window(source(u64::MAX), 0, 64, CancellationToken::new())
            .await
            .unwrap()
            .text
            .contains("completed")
    );
    assert_eq!(
        handle.source_reads.requests.lock().unwrap().last(),
        Some(&(None, 1))
    );
}

pub async fn owned_source_window_reads(execution: Execution) {
    let runtime =
        Runtime::with_execution(rsi_meta::RuntimeLimits::default(), execution.clone()).unwrap();
    let handle = Handle::new("source-owner", false);
    install_service(&runtime, vec![handle.clone()]).await;
    controller(&runtime.root(), &handle, Sink::new(false), None).await;
    let client = runtime
        .root()
        .lookup_local::<SessionControllerContract>()
        .unwrap();
    exact_reads(&client, &handle).await;
    handle.source_reads.block.store(true, Ordering::SeqCst);
    let stop = CancellationToken::new();
    drop(client.source_window(source(9), 0, 64, stop.clone()));
    until(&execution, || {
        handle.source_reads.active.load(Ordering::SeqCst) == 1
    })
    .await;
    stop.cancel();
    until(&execution, || {
        handle.source_reads.active.load(Ordering::SeqCst) == 0
    })
    .await;
    for _ in 0..4 {
        drop(client.source_window(source(9), 0, 64, CancellationToken::new()));
    }
    until(&execution, || {
        handle.source_reads.active.load(Ordering::SeqCst) == 4
    })
    .await;
    assert!(matches!(
        client
            .source_window(source(9), 0, 64, CancellationToken::new())
            .await,
        Err(SourceReadError::Session(SessionError::Capacity))
    ));
    assert!(matches!(
        client.commands().await,
        Err(SessionError::Capacity)
    ));
    assert!(handle.submissions.lock().unwrap().is_empty());
    assert!(runtime.shutdown().await.is_clean());
    assert_eq!(handle.source_reads.active.load(Ordering::SeqCst), 0);
    assert!(matches!(
        client
            .source_window(source(9), 0, 64, CancellationToken::new())
            .await,
        Err(SourceReadError::Session(SessionError::ShuttingDown))
    ));
}
