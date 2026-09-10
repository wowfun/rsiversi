use super::{LocalSessionHandle, LocalSessionService};
use futures_util::FutureExt as _;
use rsi_agent_session_protocol::SessionId;
use rsi_meta::{Deadline, Execution};
use rsi_session_protocol::{CreateSession, Result, SessionError};
use std::collections::BTreeMap;
use std::fmt;
use std::panic::AssertUnwindSafe;
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;
use tokio::sync::watch;
use tokio_util::{sync::CancellationToken, task::TaskTracker};

const CAPACITY: usize = 1024;
const DEVICE_CAPACITY: usize = 64;
const IDLE: Duration = Duration::from_hours(1);
const SWEEP: Duration = Duration::from_mins(1);
type Created = Result<Arc<LocalSessionHandle>>;

pub(super) struct Drafts {
    entries: Mutex<BTreeMap<SessionId, Arc<Entry>>>,
    execution: Execution,
    stopped: CancellationToken,
    tasks: TaskTracker,
}

impl fmt::Debug for Drafts {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Drafts").finish_non_exhaustive()
    }
}

struct Entry {
    request: CreateSession,
    device: Option<rsi_api_protocol::DeviceId>,
    state: Mutex<LeaseState>,
    result: watch::Sender<Option<Created>>,
}

struct LeaseState {
    deadline: Deadline,
    active: usize,
    retired: bool,
}

impl Entry {
    fn expired(&self) -> bool {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.active == 0 && state.deadline.has_elapsed() {
            state.retired = true;
        }
        state.retired
    }
}

impl Drop for Entry {
    fn drop(&mut self) {
        if let Some(Ok(handle)) = &*self.result.borrow() {
            // Active operations own an Arc<Entry>. The final Entry drop therefore
            // cannot overlap an operation holding the fresh-publication mutex.
            handle.expire_draft();
        }
    }
}

impl Drop for Drafts {
    fn drop(&mut self) {
        self.stopped.cancel();
    }
}

impl Drafts {
    pub(super) fn new(execution: Execution) -> Arc<Self> {
        let drafts = Arc::new(Self {
            entries: Mutex::new(BTreeMap::new()),
            execution: execution.clone(),
            stopped: CancellationToken::new(),
            tasks: TaskTracker::new(),
        });
        let weak = Arc::downgrade(&drafts);
        let stopped = drafts.stopped.clone();
        let clock = execution;
        drop(
            drafts
                .execution
                .spawn(drafts.tasks.track_future(async move {
                    loop {
                        tokio::select! {
                            biased;
                            () = stopped.cancelled() => break,
                            () = clock.sleep(SWEEP) => {
                                let Some(drafts) = weak.upgrade() else { break };
                                drafts.prune();
                            }
                        }
                    }
                })),
        );
        drafts
    }

    pub(super) fn accepting(&self) -> Result<()> {
        if self.stopped.is_cancelled() {
            Err(SessionError::ShuttingDown)
        } else {
            Ok(())
        }
    }

    pub(super) async fn stop(&self) {
        self.stopped.cancel();
        let entries = std::mem::take(
            &mut *self
                .entries
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        );
        for entry in entries.values() {
            entry
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .retired = true;
        }
        drop(entries);
        self.tasks.close();
        self.tasks.wait().await;
    }

    fn prune(&self) {
        let removed = {
            let mut entries = self
                .entries
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let expired = entries
                .iter()
                .filter(|(_, entry)| entry.expired())
                .map(|(id, _)| id.clone())
                .collect::<Vec<_>>();
            expired
                .into_iter()
                .filter_map(|id| entries.remove(&id))
                .collect::<Vec<_>>()
        };
        drop(removed);
    }

    fn remove(&self, entry: &Arc<Entry>) {
        entry
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .retired = true;
        let removed = {
            let mut entries = self
                .entries
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if entries
                .get(&entry.request.session_id)
                .is_some_and(|current| Arc::ptr_eq(current, entry))
            {
                entries.remove(&entry.request.session_id)
            } else {
                None
            }
        };
        drop(removed);
    }

    pub(super) async fn create(
        self: &Arc<Self>,
        service: LocalSessionService,
        request: CreateSession,
        device: Option<rsi_api_protocol::DeviceId>,
    ) -> Created {
        self.accepting()?;
        self.prune();
        let (entry, owner) = {
            let mut entries = self
                .entries
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            self.accepting()?;
            if let Some(entry) = entries.get(&request.session_id) {
                if entry.request != request {
                    return Err(SessionError::DraftConflict {
                        session: request.session_id.to_string(),
                    });
                }
                (entry.clone(), false)
            } else {
                if entries.len() >= CAPACITY
                    || device.as_ref().is_some_and(|device| {
                        entries
                            .values()
                            .filter(|entry| entry.device.as_ref() == Some(device))
                            .count()
                            >= DEVICE_CAPACITY
                    })
                {
                    return Err(SessionError::Capacity);
                }
                let (result, _) = watch::channel(None);
                let entry = Arc::new(Entry {
                    request: request.clone(),
                    device,
                    state: Mutex::new(LeaseState {
                        deadline: self.execution.deadline_after(IDLE),
                        active: 0,
                        retired: false,
                    }),
                    result,
                });
                entries.insert(request.session_id.clone(), entry.clone());
                (entry, true)
            }
        };
        let receiver = entry.result.subscribe();
        if owner {
            let drafts = self.clone();
            let deadline = entry
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .deadline
                .clone();
            let lease = DraftLease {
                drafts: Arc::downgrade(self),
                entry: Arc::downgrade(&entry),
            };
            drop(self.execution.spawn(self.tasks.track_future(async move {
                let result = tokio::select! {
                    biased;
                    () = drafts.stopped.cancelled() => Err(SessionError::ShuttingDown),
                    result = deadline.timeout(AssertUnwindSafe(service.prepare_draft(request, lease)).catch_unwind()) => {
                        match result {
                            Ok(result) => result.unwrap_or_else(|_| Err(SessionError::Backend("draft preparation panicked".into()))),
                            Err(_) => Err(SessionError::Backend("draft preparation deadline elapsed".into())),
                        }
                    }
                };
                entry.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner).deadline = drafts.execution.deadline_after(IDLE);
                let failed = result.is_err();
                entry.result.send_replace(Some(result));
                if failed || drafts.stopped.is_cancelled() { drafts.remove(&entry); }
            })));
        } else {
            drop(entry);
        }
        let handle = wait(receiver).await?;
        let _activity = handle.begin_activity()?;
        Ok(handle)
    }

    pub(super) async fn get(&self, id: &SessionId) -> Result<Option<Arc<LocalSessionHandle>>> {
        self.accepting()?;
        let entry = self
            .entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(id)
            .cloned();
        let Some(entry) = entry else { return Ok(None) };
        if entry.expired() {
            self.remove(&entry);
            return Ok(None);
        }
        let receiver = entry.result.subscribe();
        drop(entry);
        let handle = wait(receiver).await?;
        let _activity = handle.begin_activity()?;
        Ok(Some(handle))
    }
}

async fn wait(mut receiver: watch::Receiver<Option<Created>>) -> Created {
    loop {
        if let Some(result) = receiver.borrow_and_update().clone() {
            return result;
        }
        receiver
            .changed()
            .await
            .map_err(|_| SessionError::ShuttingDown)?;
    }
}

#[derive(Clone)]
pub(super) struct DraftLease {
    drafts: Weak<Drafts>,
    entry: Weak<Entry>,
}

impl DraftLease {
    pub(super) fn begin(&self) -> Result<Activity> {
        let drafts = self.drafts.upgrade().ok_or_else(expired)?;
        drafts.accepting()?;
        let entry = self.entry.upgrade().ok_or_else(expired)?;
        let mut state = entry
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.retired || (state.active == 0 && state.deadline.has_elapsed()) {
            state.retired = true;
            drop(state);
            drafts.remove(&entry);
            return Err(expired());
        }
        state.active = state.active.checked_add(1).ok_or(SessionError::Capacity)?;
        state.deadline = drafts.execution.deadline_after(IDLE);
        drop(state);
        Ok(Activity {
            entry,
            execution: drafts.execution.clone(),
        })
    }

    pub(super) fn published(&self) {
        if let (Some(drafts), Some(entry)) = (self.drafts.upgrade(), self.entry.upgrade()) {
            drafts.remove(&entry);
        }
    }
}

pub(super) struct Activity {
    entry: Arc<Entry>,
    execution: Execution,
}

impl Drop for Activity {
    fn drop(&mut self) {
        let mut state = self
            .entry
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.active -= 1;
        state.deadline = self.execution.deadline_after(IDLE);
    }
}

fn expired() -> SessionError {
    SessionError::NotFound("draft lease expired or its Session service retired".into())
}
