use super::*;
use rsi_agent_session_protocol::{
    ContributionId, ProjectionCursor, ProjectionEntry, ProjectionValue, SessionProjectionSnapshot,
};
use rsi_session_protocol::{ProjectionRetention, ProjectionSnapshot, ProjectionStream};
use std::sync::atomic::AtomicBool;

#[derive(Debug)]
pub struct Scenario {
    pub active: Arc<AtomicUsize>,
    pub opens: AtomicUsize,
    pub fail: Arc<AtomicBool>,
    pub changes: tokio::sync::watch::Sender<u64>,
    pub retention: ProjectionRetention,
}
impl Default for Scenario {
    fn default() -> Self {
        Self {
            active: Arc::default(),
            opens: AtomicUsize::new(0),
            fail: Arc::default(),
            changes: tokio::sync::watch::channel(0).0,
            retention: ProjectionRetention::default(),
        }
    }
}
fn snapshot(
    pool: &ProjectionRetention,
    id: &SessionId,
    revision: u64,
) -> rsi_session_protocol::Result<ProjectionSnapshot> {
    pool.reserve_capture()?.retain(
        SessionProjectionSnapshot::new(
            id.clone(),
            "a".repeat(64),
            "b".repeat(64),
            ProjectionCursor::Draft { revision },
            vec![
                ProjectionEntry::failed(
                    ContributionId::new("fixture.failed").unwrap(),
                    "isolated producer failure",
                )
                .unwrap(),
                ProjectionEntry::value(
                    ContributionId::new("fixture.good").unwrap(),
                    ProjectionValue::new(serde_json::json!({"revision":revision})).unwrap(),
                ),
            ],
        )
        .unwrap(),
    )
}
impl Scenario {
    pub fn open(&self, id: &SessionId) -> rsi_session_protocol::Result<ProjectionStream> {
        self.opens.fetch_add(1, Ordering::SeqCst);
        if self.fail.load(Ordering::SeqCst) {
            return Err(SessionError::Backend("projection unavailable".into()));
        }
        let active = Active::new(&self.active);
        let mut changes = self.changes.subscribe();
        let initial = snapshot(&self.retention, id, *changes.borrow_and_update())?;
        let state = (
            changes,
            id.clone(),
            self.retention.clone(),
            self.fail.clone(),
            active,
        );
        let changes = futures_util::stream::unfold(
            state,
            |(mut changes, id, pool, fail, active)| async move {
                changes.changed().await.ok()?;
                let revision = *changes.borrow_and_update();
                let value = if fail.load(Ordering::SeqCst) || revision == 1 {
                    Err(SessionError::Backend("projection connection lost".into()))
                } else {
                    snapshot(&pool, &id, revision)
                };
                Some((value, (changes, id, pool, fail, active)))
            },
        );
        Ok(Box::pin(
            futures_util::stream::iter([Ok(initial)]).chain(changes),
        ))
    }
}

pub async fn independent_projection_observation(execution: Execution) {
    let runtime =
        Runtime::with_execution(rsi_meta::RuntimeLimits::default(), execution.clone()).unwrap();
    let handle = Handle::new("projections", false);
    install_service(&runtime, vec![handle.clone()]).await;
    let sink = Sink::new(false);
    controller(&runtime.root(), &handle, sink.clone(), None).await;
    until(&execution, || sink.projections.lock().unwrap().is_some()).await;
    assert_eq!(handle.active_streams.load(Ordering::SeqCst), 0);
    assert!(handle.submissions.lock().unwrap().is_empty());
    assert_eq!(handle.projections.active.load(Ordering::SeqCst), 1);
    let baseline = sink.projections.lock().unwrap().clone().unwrap();
    assert_eq!(baseline.snapshot().entries().len(), 2);
    assert!(baseline.snapshot().entries()[0].failure().is_some());
    assert!(baseline.snapshot().entries()[1].view().is_some());
    handle.projections.changes.send_replace(1);
    until(&execution, || {
        sink.projections
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .snapshot()
            .cursor()
            == ProjectionCursor::Draft { revision: 1 }
    })
    .await;
    assert_eq!(handle.projections.opens.load(Ordering::SeqCst), 2);
    assert_eq!(handle.projections.active.load(Ordering::SeqCst), 1);
    drop(baseline);
    let current = sink.projections.lock().unwrap().clone().unwrap();
    assert_eq!(
        handle.projections.retention.retained_bytes(),
        current.snapshot().encoded_len().unwrap()
    );
    drop(current);
    let client = runtime
        .root()
        .lookup_local::<SessionControllerContract>()
        .unwrap();
    handle.release.add_permits(1);
    client.submit(input("after-baseline")).await.unwrap();
    until(&execution, || {
        sink.observations.load(Ordering::SeqCst) == 1
            && sink.interactions.load(Ordering::SeqCst) == 1
    })
    .await;
    handle.projections.fail.store(true, Ordering::SeqCst);
    handle.projections.changes.send_replace(2);
    until(&execution, || {
        sink.projection_stopped.load(Ordering::SeqCst) == 1
    })
    .await;
    assert_eq!(sink.stopped.load(Ordering::SeqCst), 0);
    assert_eq!(handle.active_streams.load(Ordering::SeqCst), 2);
    assert_eq!(handle.projections.active.load(Ordering::SeqCst), 0);
    assert!(runtime.shutdown().await.is_clean());
    assert_eq!(handle.active_streams.load(Ordering::SeqCst), 0);
    assert_eq!(handle.projections.retention.retained_bytes(), 0);
}
