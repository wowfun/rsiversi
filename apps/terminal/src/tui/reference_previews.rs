//! Own admitted capture replies independently of cancellable presentation reads.
use super::*;
use rsi_agent_session_protocol::{FrozenReference, ReferenceSnapshotRef, ReferenceTextPage};
use tokio::sync::{OwnedSemaphorePermit, Semaphore, oneshot};
use tokio_util::task::TaskTracker;

pub(super) struct PreviewLease {
    pub capture_id: String,
    adopted: oneshot::Sender<()>,
}
impl PreviewLease {
    pub fn adopt(self) {
        let _ = self.adopted.send(());
    }
}
struct Failed {
    handle: Arc<dyn SessionHandle>,
    snapshot: ReferenceSnapshotRef,
    _slot: OwnedSemaphorePermit,
}
pub(super) struct Previews {
    tasks: TaskTracker,
    slots: Arc<Semaphore>,
    jobs: Arc<Semaphore>,
    failed: std::sync::Mutex<Vec<Failed>>,
    notice: std::sync::Mutex<Option<String>>,
}
impl Default for Previews {
    fn default() -> Self {
        Self {
            tasks: TaskTracker::new(),
            slots: Arc::new(Semaphore::new(4)),
            jobs: Arc::new(Semaphore::new(4)),
            failed: Default::default(),
            notice: Default::default(),
        }
    }
}
impl Previews {
    pub fn notice(&self) -> Option<String> {
        self.notice.lock().expect("reference cleanup").take()
    }
    async fn retry_cleanup(&self) -> Result<()> {
        let failed = std::mem::take(&mut *self.failed.lock().expect("reference cleanup"));
        let mut problem = None;
        for item in failed {
            if let Err(failure) = item.handle.release_reference(item.snapshot.clone()).await {
                problem = Some(error(format!("Reference cleanup failed: {failure}")));
                self.failed.lock().expect("reference cleanup").push(item);
            }
        }
        problem.map_or(Ok(()), Err)
    }
    pub async fn capture(
        self: Arc<Self>,
        handle: Arc<dyn SessionHandle>,
        capture: impl std::future::Future<Output = Result<FrozenReference>> + Send + 'static,
    ) -> Result<(ReferenceTextPage, PreviewLease)> {
        let job = self
            .jobs
            .clone()
            .try_acquire_owned()
            .map_err(|_| error("Reference previews are still settling"))?;
        let (send, receive) = oneshot::channel();
        let owner = self.clone();
        tokio::spawn(self.tasks.track_future(async move {
            let _job = job;
            if let Err(problem) = owner.retry_cleanup().await {
                *owner.notice.lock().expect("reference cleanup") = Some(problem.to_string());
            }
            if send.is_closed() {
                return;
            }
            let Ok(slot) = owner.slots.clone().try_acquire_owned() else {
                let _ = send.send(Err(error("Reference previews are still settling")));
                return;
            };
            let reference = match capture.await {
                Ok(reference) => reference,
                Err(problem) => {
                    let _ = send.send(Err(problem));
                    return;
                }
            };
            let snapshot = reference.snapshot.clone();
            let (adopt, adopted) = oneshot::channel();
            let reply = handle
                .preview_reference(reference, 0, 8192)
                .await
                .map_err(error)
                .map(|page| {
                    (
                        page,
                        PreviewLease {
                            capture_id: snapshot.capture_id.clone(),
                            adopted: adopt,
                        },
                    )
                });
            let _ = send.send(reply);
            if adopted.await.is_ok() {
                return;
            }
            if let Err(failure) = handle.release_reference(snapshot.clone()).await {
                *owner.notice.lock().expect("reference cleanup") = Some(format!(
                    "Reference cleanup failed: {failure}. Retry Capture to release it."
                ));
                owner
                    .failed
                    .lock()
                    .expect("reference cleanup")
                    .push(Failed {
                        handle,
                        snapshot,
                        _slot: slot,
                    });
            }
        }));
        receive
            .await
            .map_err(|_| error("Reference worker stopped"))?
    }
    pub async fn close(&self) -> Result<()> {
        self.tasks.close();
        self.tasks.wait().await;
        self.retry_cleanup().await
    }
}
