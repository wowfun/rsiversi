use crate::{
    git::{Capture, Comparison, Git, Scratch},
    scratch_root::Root,
};
use async_trait::async_trait;
use rsi_agent_turn_protocol::{
    ControlledWorkStatus, ExecutionInterval, ExecutionObservationEnd, ExecutionObservationStart,
    ExecutionObserver, TurnService,
};
use rsi_api_protocol::{ApiError, Result};
use rsi_execution::{ExecutionLease, ExecutionLocation, ExecutionOperation};
use rsi_meta::Execution;
use rsi_workspace_review_api::{
    ConversationIdentity, Omission, OmissionKind, Phase, Reply, Request, Scope, Summary,
};
use std::{
    collections::BTreeMap,
    path::PathBuf,
    sync::{Arc, Mutex},
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::sync::Semaphore;
use tokio_util::{sync::CancellationToken, task::TaskTracker};

fn invalid(e: impl std::fmt::Display) -> ApiError {
    ApiError::Invalid(e.to_string())
}
fn id() -> Result<String> {
    let mut bytes = [0; 16];
    getrandom::fill(&mut bytes).map_err(invalid)?;
    Ok(hex::encode(bytes))
}
fn now() -> String {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(u128::from(u64::MAX))
        .to_string()
}
#[derive(Debug)]
struct Changes {
    scratch: Scratch,
    comparison: Comparison,
    patch: tokio::sync::Mutex<Option<(String, Arc<str>)>>,
}
#[derive(Debug)]
struct Entry {
    summary: Summary,
    status: EntryStatus,
    runtime: Option<Arc<Changes>>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum EntryStatus {
    Baseline,
    Recording,
    Finished,
}
#[derive(Debug)]
struct State {
    closed: bool,
    entries: BTreeMap<String, Entry>,
}
/// Ordinary product owner for non-authoritative interval evidence.
#[derive(Debug)]
pub struct WorkspaceReview {
    turns: Arc<dyn TurnService>,
    root: Root,
    git: Git,
    execution: Execution,
    state: Mutex<State>,
    workers: Arc<Semaphore>,
    reads: Arc<Semaphore>,
    stop: CancellationToken,
    tasks: TaskTracker,
}
#[derive(Debug, Default)]
struct IntervalState {
    admission: Option<ExecutionOperation>,
    finished: bool,
    before: Option<(Scratch, Capture)>,
    omissions: Vec<Omission>,
}
#[derive(Clone, Debug)]
struct Interval {
    owner: Arc<WorkspaceReview>,
    id: String,
    workspace: PathBuf,
    execution: Option<ExecutionLease>,
    recovered: bool,
    state: Arc<tokio::sync::Mutex<IntervalState>>,
}
enum Stage {
    Begin,
    End(ExecutionObservationEnd),
}
#[derive(Debug)]
pub(super) struct Observer(pub Arc<WorkspaceReview>);
#[async_trait]
impl ExecutionObserver for Observer {
    async fn observe(
        &self,
        start: ExecutionObservationStart,
        cancellation: CancellationToken,
    ) -> std::result::Result<Arc<dyn ExecutionInterval>, String> {
        if cancellation.is_cancelled() {
            return Err("workspace observation admission cancelled".into());
        }
        let interval = self.0.admit(start).map_err(|error| error.to_string())?;
        Ok(Arc::new(interval))
    }
}
fn omission(rows: &mut Vec<Omission>, kind: OmissionKind, count: u32) {
    if let Some(row) = rows.iter_mut().find(|row| row.kind == kind) {
        row.count = row.count.saturating_add(count);
    } else {
        rows.push(Omission { kind, count });
    }
    rows.sort_by_key(|row| row.kind);
}
impl WorkspaceReview {
    pub(super) fn open(
        turns: Arc<dyn TurnService>,
        root: Root,
        git: Git,
        execution: Execution,
    ) -> Arc<Self> {
        let tasks = git.tasks.clone();
        Arc::new(Self {
            turns,
            root,
            git,
            execution,
            state: Mutex::new(State {
                closed: false,
                entries: BTreeMap::new(),
            }),
            workers: Arc::new(Semaphore::new(2)),
            reads: Arc::new(Semaphore::new(2)),
            stop: CancellationToken::new(),
            tasks,
        })
    }
    fn admit(self: &Arc<Self>, start: ExecutionObservationStart) -> Result<Interval> {
        let admission = match &start.execution {
            Some(execution)
                if execution.binding().location() == start.header.coordinates().location() =>
            {
                Some(execution.admit().map_err(|error| match error {
                    rsi_process::ProcessError::Api(error) => error,
                    rsi_process::ProcessError::Capacity => ApiError::Capacity,
                    _ => ApiError::Unauthorized,
                })?)
            }
            None if *start.header.coordinates().location() == ExecutionLocation::Local => None,
            _ => return Err(ApiError::Unauthorized),
        };
        let mut state = self.state.lock().expect("review admission");
        if state.closed {
            return Err(ApiError::ShuttingDown);
        }
        let id = id()?;
        if state.entries.contains_key(&id) {
            return Err(ApiError::Capacity);
        }
        let workspace = PathBuf::from(start.header.canonical_cwd());
        let scope = Scope {
            workspace: rsi_workspace_protocol::WorkspaceKey::from_coordinates(
                start.header.coordinates(),
            ),
            conversation: ConversationIdentity::Native(start.header.session_id().clone()),
        };
        let summary = Summary {
            id: id.clone(),
            scope,
            turn: Some(start.turn),
            execution: start.claim.to_string(),
            accepted_seq: start.accepted_seq.to_string(),
            live_seq: start.live_seq.to_string(),
            started_ms: now(),
            finished_ms: None,
            phase: Phase::Pending,
            changed_files: 0,
            added_lines: 0,
            removed_lines: 0,
            omissions: vec![],
        };
        state.entries.insert(
            id.clone(),
            Entry {
                summary,
                status: EntryStatus::Baseline,
                runtime: None,
            },
        );
        Ok(Interval {
            owner: self.clone(),
            id,
            workspace,
            execution: start.execution,
            recovered: start.recovered,
            state: Arc::new(tokio::sync::Mutex::new(IntervalState {
                admission,
                ..IntervalState::default()
            })),
        })
    }
    fn take_oldest_scratch(&self) -> Option<Scratch> {
        let mut state = self.state.lock().expect("review scratch eviction");
        let id = state
            .entries
            .iter()
            .filter(|(_, entry)| {
                entry
                    .runtime
                    .as_ref()
                    .is_some_and(|changes| Arc::strong_count(changes) == 1)
            })
            .min_by_key(|(_, entry)| (entry.summary.started_ms.len(), &entry.summary.started_ms))
            .map(|(id, _)| id.clone())?;
        let entry = state.entries.remove(&id)?;
        Arc::into_inner(entry.runtime?).map(|changes| changes.scratch)
    }
    /// Returns finite evidence after the product adapter has authorized the exact scope.
    ///
    /// # Panics
    /// Panics if an earlier failure poisoned owner state.
    pub async fn read(
        self: &Arc<Self>,
        request: Request,
        admission: ExecutionOperation,
        cancellation: CancellationToken,
    ) -> Result<Reply> {
        request.validate()?;
        let stop = self.stop.child_token();
        let _guard = stop.clone().drop_guard();
        let task = {
            let state = self.state.lock().expect("review read admission");
            if state.closed {
                return Err(ApiError::ShuttingDown);
            }
            let permit = self
                .reads
                .clone()
                .try_acquire_owned()
                .map_err(|_| ApiError::Capacity)?;
            let owner = self.clone();
            let token = stop.clone();
            // Registration and close share admission, so shutdown cannot miss this task.
            self.execution.spawn(self.tasks.track_future(async move {
                let _permit = permit;
                let _admission = admission;
                let reply = owner.read_inner(&request, &token).await?;
                rsi_workspace_review_api::validate_reply(&request, &reply)?;
                Ok(reply)
            }))
        };
        tokio::select! {biased;()=cancellation.cancelled()=>Err(ApiError::ShuttingDown),()=self.stop.cancelled()=>Err(ApiError::ShuttingDown),result=tokio::time::timeout(Duration::from_secs(30),task)=>result.map_err(|_|ApiError::Unavailable)?.map_err(|_|ApiError::Unavailable)?}
    }
    #[expect(
        clippy::too_many_lines,
        reason = "closed read variants share one authorization and admission boundary"
    )]
    async fn read_inner(&self, request: &Request, stop: &CancellationToken) -> Result<Reply> {
        if stop.is_cancelled() {
            return Err(ApiError::ShuttingDown);
        }
        if let Request::List { scope, after } = request {
            let state = self.state.lock().expect("review summaries");
            let mut items = Vec::new();
            let mut bytes = 0;
            let mut more = false;
            for (id, entry) in &state.entries {
                if entry.status == EntryStatus::Baseline
                    || &entry.summary.scope != scope
                    || after.as_ref().is_some_and(|after| id <= after)
                {
                    continue;
                }
                let length = serde_json::to_vec(&entry.summary).map_err(invalid)?.len();
                if items.len() >= 32 || bytes + length > 900 * 1024 {
                    more = true;
                    break;
                }
                bytes += length;
                items.push(entry.summary.clone());
            }
            let next = if more {
                items.last().map(|s| s.id.clone())
            } else {
                None
            };
            return Ok(Reply::Summaries { items, next });
        }
        let (id, scope) = match request {
            Request::Files { id, scope, .. } | Request::Diff { id, scope, .. } => (id, scope),
            Request::List { .. } => unreachable!(),
        };
        let runtime = {
            let state = self.state.lock().expect("review interval");
            match state.entries.get(id) {
                Some(entry) if &entry.summary.scope != scope => {
                    return Err(invalid("review source mismatch"));
                }
                Some(entry) if entry.status != EntryStatus::Finished => {
                    return Err(ApiError::Unavailable);
                }
                Some(entry) => entry.runtime.clone(),
                None => None,
            }
        };
        let Some(runtime) = runtime else {
            return Ok(Reply::Expired { id: id.clone() });
        };
        match request {
            Request::Files { offset, .. } => {
                let changed = &runtime.comparison.files;
                if *offset > changed.len() {
                    return Err(invalid("file cursor exceeds comparison"));
                }
                let mut files = Vec::new();
                let mut bytes = 0;
                for file in changed.iter().skip(*offset).take(64) {
                    let length = serde_json::to_vec(file).map_err(invalid)?.len();
                    if bytes + length > 240 * 1024 {
                        break;
                    }
                    bytes += length;
                    files.push(file.clone());
                }
                Ok(Reply::Files {
                    id: id.clone(),
                    offset: *offset,
                    has_more: offset + files.len() < changed.len(),
                    files,
                })
            }
            Request::Diff { path, offset, .. } => {
                let index = runtime
                    .comparison
                    .files
                    .binary_search_by(|file| file.path.cmp(path))
                    .map_err(|_| ApiError::Unavailable)?;
                let file = &runtime.comparison.files[index];
                let text = {
                    let mut patch = runtime.patch.lock().await;
                    if let Some((cached_path, text)) = &*patch
                        && cached_path == path
                    {
                        text.clone()
                    } else {
                        let text: Arc<str> = self
                            .git
                            .diff(&runtime.scratch, &runtime.comparison, file, stop)
                            .await
                            .map_err(|_| ApiError::Unavailable)?
                            .into();
                        *patch = Some((path.clone(), text.clone()));
                        text
                    }
                };
                if *offset > text.len() {
                    return Err(invalid("diff cursor exceeds captured text"));
                }
                let mut start = *offset;
                while !text.is_char_boundary(start) {
                    start -= 1;
                }
                let mut end = (start + 65536).min(text.len());
                while !text.is_char_boundary(end) {
                    end -= 1;
                }
                Ok(Reply::Diff {
                    id: id.clone(),
                    path: path.clone(),
                    offset: start,
                    next_offset: end,
                    has_more: end < text.len(),
                    text: text[start..end].into(),
                })
            }
            Request::List { .. } => unreachable!(),
        }
    }
    /// Cancels and drains actual work before releasing scratch and its quota.
    ///
    /// # Panics
    /// Panics if an earlier failure poisoned owner state.
    pub async fn close(&self) {
        {
            let mut state = self.state.lock().expect("review close");
            state.closed = true;
            self.stop.cancel();
            self.workers.close();
            self.reads.close();
            self.tasks.close();
        }
        self.tasks.wait().await;
        self.state.lock().expect("review cleanup").entries.clear();
        self.tasks.wait().await;
    }
}
impl Interval {
    async fn capture_permit(
        &self,
        stop: &CancellationToken,
    ) -> std::result::Result<tokio::sync::OwnedSemaphorePermit, ()> {
        tokio::select! { biased;
            () = stop.cancelled() => Err(()),
            permit = self.owner.workers.clone().acquire_owned() => permit.map_err(|_| ()),
        }
    }
    async fn begin_work(&self, stop: &CancellationToken) {
        let mut inner = self.state.lock().await;
        if inner.finished {
            return;
        }
        if self.recovered {
            omission(
                &mut inner.omissions,
                OmissionKind::RecoveredWithoutBaseline,
                1,
            );
        }
        let Ok(_permit) = self.capture_permit(stop).await else {
            omission(&mut inner.omissions, OmissionKind::Deadline, 1);
            return;
        };
        let mut scratch = self.owner.git.scratch();
        match self.capture(&mut scratch, stop).await {
            Ok(before) => inner.before = Some((scratch, before)),
            Err(reason) => omission(&mut inner.omissions, reason, 1),
        }
    }
    async fn capture(
        &self,
        scratch: &mut Scratch,
        stop: &CancellationToken,
    ) -> crate::git::Result<Capture> {
        self.owner
            .git
            .capture(
                &self.workspace,
                &self.owner.root.path,
                scratch,
                stop,
                self.execution.as_ref(),
                &|| self.owner.take_oldest_scratch(),
            )
            .await
    }
    async fn end_work(&self, evidence: ExecutionObservationEnd, stop: &CancellationToken) {
        let mut inner = self.state.lock().await;
        if inner.finished {
            return;
        }
        inner.finished = true;
        let _admission = inner.admission.take();
        {
            let mut state = self.owner.state.lock().expect("review end");
            if state.entries[&self.id].status == EntryStatus::Baseline {
                state.entries.remove(&self.id);
                inner.before.take();
                return;
            }
        }
        if !evidence.begin_completed {
            omission(&mut inner.omissions, OmissionKind::MissingBaseline, 1);
        }
        if evidence.controlled_work != ControlledWorkStatus::Settled {
            omission(&mut inner.omissions, OmissionKind::Unsettled, 1);
        }
        let mut ready = None;
        if let Some((mut scratch, before)) = inner.before.take() {
            for row in &before.omissions {
                omission(&mut inner.omissions, row.kind, row.count);
            }
            if evidence.begin_completed {
                if let Ok(_permit) = self.capture_permit(stop).await {
                    let comparison = async {
                        let after = self.capture(&mut scratch, stop).await?;
                        for row in &after.omissions {
                            omission(&mut inner.omissions, row.kind, row.count);
                        }
                        self.owner
                            .git
                            .compare(&scratch, &before, &after, stop)
                            .await
                    }
                    .await;
                    match comparison {
                        Ok(Some(comparison)) if !comparison.files.is_empty() => {
                            ready = Some(Arc::new(Changes {
                                scratch,
                                comparison,
                                patch: tokio::sync::Mutex::new(None),
                            }));
                        }
                        Ok(_) => {}
                        Err(reason) => omission(&mut inner.omissions, reason, 1),
                    }
                } else {
                    omission(&mut inner.omissions, OmissionKind::Deadline, 1);
                }
            }
        } else {
            omission(&mut inner.omissions, OmissionKind::MissingBaseline, 1);
        }
        let Some(changes) = ready else {
            self.owner
                .state
                .lock()
                .expect("review end")
                .entries
                .remove(&self.id);
            return;
        };
        let (session, turn) = {
            let mut state = self.owner.state.lock().expect("review publication");
            let entry = state.entries.get_mut(&self.id).expect("admitted review");
            let summary = &mut entry.summary;
            summary.finished_ms = Some(now());
            summary.omissions = std::mem::take(&mut inner.omissions);
            summary.phase = if summary.omissions.is_empty() {
                Phase::Complete
            } else {
                Phase::Partial
            };
            summary.changed_files =
                u32::try_from(changes.comparison.files.len()).expect("bounded changed files");
            for file in &changes.comparison.files {
                summary.added_lines = summary.added_lines.saturating_add(file.added);
                summary.removed_lines = summary.removed_lines.saturating_add(file.removed);
            }
            entry.status = EntryStatus::Finished;
            entry.runtime = Some(changes);
            let (ConversationIdentity::Native(session), Some(turn)) =
                (&summary.scope.conversation, &summary.turn)
            else {
                unreachable!("execution intervals belong to a native turn")
            };
            (session.clone(), turn.clone())
        };
        if let Err(error) = self
            .owner
            .turns
            .record_workspace_changes(&session, &turn)
            .await
        {
            eprintln!("Could not record workspace changes for {session}/{turn}: {error}");
        }
    }
    async fn retained(&self, operation: Stage, cancellation: CancellationToken) {
        let stop = self.owner.stop.child_token();
        let _guard = stop.clone().drop_guard();
        let worker_stop = stop.clone();
        let interval = self.clone();
        let task = {
            let state = self.owner.state.lock().expect("review stage admission");
            if state.closed {
                return;
            }
            self.owner
                .execution
                .spawn(self.owner.tasks.track_future(async move {
                    match operation {
                        Stage::Begin => interval.begin_work(&worker_stop).await,
                        Stage::End(evidence) => interval.end_work(evidence, &worker_stop).await,
                    }
                }))
        };
        tokio::select! {biased;()=cancellation.cancelled()=>{},()=self.owner.stop.cancelled()=>{},_=task=>{}}
    }
}
#[async_trait]
impl ExecutionInterval for Interval {
    async fn begin(&self, cancellation: CancellationToken) {
        self.retained(Stage::Begin, cancellation).await;
    }
    async fn first_tool(&self, _: CancellationToken) {
        let mut state = self.owner.state.lock().expect("review tool");
        if !state.closed
            && let Some(entry) = state.entries.get_mut(&self.id)
            && entry.status == EntryStatus::Baseline
        {
            entry.status = EntryStatus::Recording;
        }
    }
    async fn end(&self, evidence: ExecutionObservationEnd, cancellation: CancellationToken) {
        self.retained(Stage::End(evidence), cancellation).await;
    }
}

#[cfg(all(test, target_os = "linux"))]
#[path = "owner_tests.rs"]
mod tests;
