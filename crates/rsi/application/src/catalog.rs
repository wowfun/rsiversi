use rsi_host::{Host, ProfileControl, ProfileHealth, ProfileProgram, ProfileUpdateHandle};
use rsi_meta::{Execution, LocalContract};
use std::sync::{Arc, Mutex};
use tokio::sync::watch;
use tokio_util::{sync::CancellationToken, task::TaskTracker};

/// Product-owned immutable role catalogs, independent of Session and presentation layout.
pub trait ProfileCatalogSource: std::fmt::Debug + Send + Sync + 'static {
    /// Captures a complete frozen Host without activating any plugin.
    fn snapshot(&self) -> rsi_host::Result<Arc<Host>>;
    /// Coalesced source changes; capture again after notification.
    fn changes(&self) -> watch::Receiver<u64>;
}

/// Local catalog view selected by the product for this application or surface.
#[derive(Debug)]
pub struct ProfileCatalogContract;
impl LocalContract for ProfileCatalogContract {
    const KEY: &'static str = "rsi.application.catalog";
    type Service = dyn ProfileCatalogSource;
}

#[derive(Debug)]
pub(crate) struct CatalogFollower {
    stop: CancellationToken,
    tasks: TaskTracker,
    diagnostic: Arc<Mutex<Option<String>>>,
}
impl CatalogFollower {
    pub(crate) fn start(
        execution: &Execution,
        source: Arc<dyn ProfileCatalogSource>,
        changes: watch::Receiver<u64>,
        program: ProfileProgram,
        updater: ProfileUpdateHandle,
        control: &dyn ProfileControl,
        refresh_initial: bool,
    ) -> Self {
        let stop = CancellationToken::new();
        let tasks = TaskTracker::new();
        let diagnostic = Arc::new(Mutex::new(None));
        let worker = Worker {
            execution: execution.clone(),
            source,
            changes,
            program,
            updater,
            status: control.subscribe(),
            stop: stop.clone(),
            diagnostic: diagnostic.clone(),
            refresh_initial,
        };
        drop(execution.spawn(tasks.track_future(worker.run())));
        tasks.close();
        Self {
            stop,
            tasks,
            diagnostic,
        }
    }
    pub(crate) async fn close(&self) {
        self.stop.cancel();
        self.tasks.wait().await;
    }
    pub(crate) fn diagnostic(&self) -> Option<String> {
        self.diagnostic
            .lock()
            .expect("catalog diagnostic poisoned")
            .clone()
    }
}
impl Drop for CatalogFollower {
    fn drop(&mut self) {
        self.stop.cancel();
    }
}

struct Worker {
    execution: Execution,
    source: Arc<dyn ProfileCatalogSource>,
    changes: watch::Receiver<u64>,
    program: ProfileProgram,
    updater: ProfileUpdateHandle,
    status: watch::Receiver<rsi_host::ProfileStatus>,
    stop: CancellationToken,
    diagnostic: Arc<Mutex<Option<String>>>,
    refresh_initial: bool,
}
impl Worker {
    fn stopped(&self) -> bool {
        self.stop.is_cancelled() || self.status.borrow().health() == ProfileHealth::Stopped
    }
    fn report(&self, error: Option<String>) {
        let error = error.map(|mut value| {
            let mut limit = value.len().min(4096);
            while !value.is_char_boundary(limit) {
                limit -= 1;
            }
            value.truncate(limit);
            value
        });
        *self.diagnostic.lock().expect("catalog diagnostic poisoned") = error;
    }
    async fn run(mut self) {
        loop {
            if self.stopped() {
                return;
            }
            if !std::mem::take(&mut self.refresh_initial) {
                tokio::select! {
                    biased;
                    () = self.stop.cancelled() => return,
                    status = self.status.changed() => { if status.is_err() { return; } continue; }
                    change = self.changes.changed() => if change.is_err() { return; },
                }
            }
            let candidate = self
                .execution
                .prepare({
                    let source = self.source.clone();
                    let program = self.program.clone();
                    move || source.snapshot()?.profile_input(program)
                })
                .await;
            if self.stopped() {
                return;
            }
            let input = match candidate {
                Ok(Ok(input)) => input,
                Ok(Err(error)) => {
                    self.report(Some(error.to_string()));
                    continue;
                }
                Err(error) => {
                    self.report(Some(error.to_string()));
                    continue;
                }
            };
            loop {
                if self.stopped() {
                    return;
                }
                match self
                    .updater
                    .submit(self.updater.input_revision(), input.clone())
                {
                    Ok(ticket) => {
                        match ticket.wait().await {
                            Ok(_) => self.report(None),
                            Err(rsi_host::ProfileError::InputConflict { .. }) => continue,
                            Err(error) => self.report(Some(error.to_string())),
                        }
                        break;
                    }
                    Err(rsi_host::ProfileError::Busy) => {
                        tokio::select! {
                            () = self.stop.cancelled() => return,
                            _ = self.status.changed() => {},
                            () = self.execution.sleep(std::time::Duration::from_millis(10)) => {},
                        }
                    }
                    Err(error) => {
                        self.report(Some(error.to_string()));
                        break;
                    }
                }
            }
        }
    }
}
