use std::sync::mpsc;
use std::time::Duration;

// Exact manual-reload counts must not include independently scheduled watcher
// reloads. Tokio's documented blocking-task inhibition prevents paused time
// from auto-advancing while the real preparation worker is idle between calls.
pub struct ManualClock {
    release: Option<mpsc::Sender<()>>,
    held: Option<tokio::task::JoinHandle<bool>>,
}

pub async fn hold() -> ManualClock {
    tokio::time::pause();
    let (release, released) = mpsc::channel();
    let (entered, started) = tokio::sync::oneshot::channel();
    let held = tokio::task::spawn_blocking(move || {
        entered.send(()).unwrap();
        matches!(
            released.recv_timeout(Duration::from_secs(15)),
            Err(mpsc::RecvTimeoutError::Disconnected)
        )
    });
    started.await.unwrap();
    ManualClock {
        release: Some(release),
        held: Some(held),
    }
}

impl ManualClock {
    pub async fn finish(mut self) {
        self.release.take();
        assert!(
            self.held.take().unwrap().await.unwrap(),
            "manual reload exceeded its wall-clock fixture deadline"
        );
    }
}

impl Drop for ManualClock {
    fn drop(&mut self) {
        // Releasing the sender also lets Tokio join the blocking task after a
        // test panic. The task has its own bound if the test itself gets stuck.
        self.release.take();
    }
}
