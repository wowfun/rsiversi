use crate::{
    git::{Capture, Comparison, Git, Scratch},
    scratch_root::Root,
};
use async_trait::async_trait;
use rsi_agent_turn_protocol::{
    ControlledWorkStatus, ExecutionInterval, ExecutionObservationEnd, ExecutionObservationStart,
    ExecutionObserver,
};
use rsi_api_protocol::{ApiError, Result};
use rsi_meta::Execution;
use rsi_storage_domain::Domain;
use rsi_workspace_review_api::{
    ConversationIdentity, Omission, OmissionKind, Phase, Reply, Request, Scope, Summary,
};
use sha2::{Digest as _, Sha256};
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
struct Ready {
    scratch: Scratch,
    comparison: Comparison,
    patch: tokio::sync::Mutex<Option<(String, Arc<str>)>>,
}
#[derive(Debug)]
struct Entry {
    summary: Summary,
    active: bool,
    runtime: Option<Arc<Ready>>,
}
#[derive(Debug)]
struct State {
    closed: bool,
    uncertain: bool,
    entries: BTreeMap<String, Entry>,
}
/// Ordinary product owner for non-authoritative interval evidence.
#[derive(Debug)]
pub struct WorkspaceReview {
    domain: Arc<dyn Domain>,
    root: Root,
    git: Git,
    execution: Execution,
    epoch: String,
    state: Mutex<State>,
    writer: tokio::sync::Mutex<()>,
    workers: Arc<Semaphore>,
    reads: Arc<Semaphore>,
    stop: CancellationToken,
    tasks: TaskTracker,
}
#[derive(Debug, Default)]
struct IntervalState {
    finished: bool,
    scratch: Option<Scratch>,
    before: Option<Capture>,
    omissions: Vec<Omission>,
}
#[derive(Clone, Debug)]
struct Interval {
    owner: Arc<WorkspaceReview>,
    id: String,
    workspace: PathBuf,
    recovered: bool,
    state: Arc<tokio::sync::Mutex<IntervalState>>,
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
        self.0
            .admit(start)
            .map(|v| Arc::new(v) as Arc<dyn ExecutionInterval>)
            .map_err(|e| e.to_string())
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
    pub(super) async fn open(
        domain: Arc<dyn Domain>,
        root: Root,
        git: Git,
        execution: Execution,
    ) -> Result<Arc<Self>> {
        let tasks = git.tasks.clone();
        let mut entries = BTreeMap::new();
        for (key, value) in domain.snapshot().await {
            let summary: Summary = serde_json::from_value(value).map_err(invalid)?;
            summary.validate()?;
            if key != summary.id {
                return Err(invalid("review summary key mismatch"));
            }
            entries.insert(
                key,
                Entry {
                    summary,
                    active: false,
                    runtime: None,
                },
            );
        }
        Ok(Arc::new(Self {
            domain,
            root,
            git,
            execution,
            epoch: id()?,
            state: Mutex::new(State {
                closed: false,
                uncertain: false,
                entries,
            }),
            writer: tokio::sync::Mutex::new(()),
            workers: Arc::new(Semaphore::new(2)),
            reads: Arc::new(Semaphore::new(2)),
            stop: CancellationToken::new(),
            tasks,
        }))
    }
    fn admit(self: &Arc<Self>, start: ExecutionObservationStart) -> Result<Interval> {
        let mut state = self.state.lock().expect("review admission");
        if state.closed {
            return Err(ApiError::ShuttingDown);
        }
        if state.uncertain {
            return Err(ApiError::OutcomeUnknown);
        }
        if state.entries.len() >= 8192 || state.entries.values().filter(|e| e.active).count() >= 8 {
            return Err(ApiError::Capacity);
        }
        // Retire oldest finished runtime material while preserving its durable summary.
        while state
            .entries
            .values()
            .filter(|e| e.runtime.is_some() || e.active)
            .count()
            >= 8
        {
            let oldest = state
                .entries
                .iter()
                .filter(|(_, e)| !e.active && e.runtime.is_some())
                .min_by_key(|(_, e)| {
                    rsi_workspace_review_api::decimal(&e.summary.started_ms).unwrap_or(0)
                })
                .map(|(id, _)| id.clone());
            let Some(oldest) = oldest else {
                break;
            };
            state.entries.get_mut(&oldest).unwrap().runtime.take();
        }
        let id = id()?;
        if state.entries.contains_key(&id) {
            return Err(ApiError::Capacity);
        }
        let workspace = PathBuf::from(start.header.canonical_cwd());
        let scope = Scope {
            workspace: rsi_workspace_protocol::WorkspaceId::parse(hex::encode(Sha256::digest(
                start.header.canonical_cwd().as_bytes(),
            )))
            .map_err(invalid)?,
            conversation: ConversationIdentity::Native(start.header.session_id().clone()),
        };
        let summary = Summary {
            id: id.clone(),
            epoch: self.epoch.clone(),
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
        summary.validate()?;
        state.entries.insert(
            id.clone(),
            Entry {
                summary,
                active: true,
                runtime: None,
            },
        );
        Ok(Interval {
            owner: self.clone(),
            id,
            workspace,
            recovered: start.recovered,
            state: Arc::new(tokio::sync::Mutex::new(IntervalState::default())),
        })
    }
    async fn publish(
        &self,
        summary: Summary,
        runtime: Option<Arc<Ready>>,
        active: bool,
    ) -> Result<()> {
        summary.validate()?;
        let _writer = self.writer.lock().await;
        if self.state.lock().expect("review writer").uncertain {
            return Err(ApiError::OutcomeUnknown);
        }
        if self
            .domain
            .put(
                &summary.id,
                serde_json::to_value(&summary).map_err(invalid)?,
            )
            .await
            .is_err()
        {
            self.state.lock().expect("review write failure").uncertain = true;
            return Err(ApiError::OutcomeUnknown);
        }
        let mut state = self.state.lock().expect("review publication");
        state.entries.insert(
            summary.id.clone(),
            Entry {
                summary,
                active,
                runtime,
            },
        );
        Ok(())
    }
    /// Returns finite evidence after the product adapter has authorized the exact scope.
    ///
    /// # Panics
    /// Panics if an earlier failure poisoned owner state.
    pub async fn read(
        self: &Arc<Self>,
        request: Request,
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
            if state.uncertain {
                return Err(ApiError::OutcomeUnknown);
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
                if &entry.summary.scope != scope || after.as_ref().is_some_and(|after| id <= after)
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
            return Ok(Reply::Summaries {
                epoch: self.epoch.clone(),
                items,
                next,
            });
        }
        let (id, scope) = match request {
            Request::Files { id, scope, .. } | Request::Diff { id, scope, .. } => (id, scope),
            Request::List { .. } => unreachable!(),
        };
        let runtime = {
            let state = self.state.lock().expect("review interval");
            let entry = state.entries.get(id).ok_or(ApiError::Unavailable)?;
            if &entry.summary.scope != scope {
                return Err(invalid("review source mismatch"));
            }
            if entry.active {
                return Err(ApiError::Unavailable);
            }
            entry.runtime.clone()
        };
        let Some(runtime) = runtime else {
            return Ok(Reply::Expired { id: id.clone() });
        };
        match request {
            Request::Files { offset, .. } => {
                if *offset > runtime.comparison.files.len() {
                    return Err(invalid("file cursor exceeds comparison"));
                }
                let mut files = Vec::new();
                let mut bytes = 0;
                for file in runtime.comparison.files.iter().skip(*offset).take(64) {
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
                    has_more: offset + files.len() < runtime.comparison.files.len(),
                    files,
                })
            }
            Request::Diff { path, offset, .. } => {
                let file = runtime
                    .comparison
                    .files
                    .iter()
                    .find(|f| &f.path == path)
                    .ok_or(ApiError::Unavailable)?;
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
        {
            let mut state = self.state.lock().expect("review cleanup");
            for entry in state.entries.values_mut() {
                entry.runtime.take();
            }
        }
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
        let summary = self.owner.state.lock().expect("review begin").entries[&self.id]
            .summary
            .clone();
        if self.owner.publish(summary, None, true).await.is_err() {
            omission(&mut inner.omissions, OmissionKind::MissingBaseline, 1);
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
        match self.owner.git.initialize(&self.owner.root.path, stop).await {
            Ok(mut scratch) => {
                let before = self
                    .owner
                    .git
                    .capture(&self.workspace, &mut scratch, stop)
                    .await;
                inner.before = Some(before);
                inner.scratch = Some(scratch);
            }
            Err(reason) => omission(&mut inner.omissions, reason, 1),
        }
    }
    async fn end_work(&self, evidence: ExecutionObservationEnd, stop: &CancellationToken) {
        let mut inner = self.state.lock().await;
        if inner.finished {
            return;
        }
        inner.finished = true;
        if !evidence.begin_completed {
            omission(&mut inner.omissions, OmissionKind::MissingBaseline, 1);
        }
        if evidence.controlled_work != ControlledWorkStatus::Settled {
            omission(&mut inner.omissions, OmissionKind::Unsettled, 1);
        }
        let mut ready = None;
        if let (Some(mut scratch), Some(before)) = (inner.scratch.take(), inner.before.take()) {
            for row in &before.omissions {
                omission(&mut inner.omissions, row.kind, row.count);
            }
            if evidence.begin_completed {
                if let Ok(_permit) = self.capture_permit(stop).await {
                    let after = self
                        .owner
                        .git
                        .capture(&self.workspace, &mut scratch, stop)
                        .await;
                    for row in &after.omissions {
                        omission(&mut inner.omissions, row.kind, row.count);
                    }
                    match self
                        .owner
                        .git
                        .compare(&scratch, &before, &after, stop)
                        .await
                    {
                        Ok(comparison) => {
                            ready = Some(Arc::new(Ready {
                                scratch,
                                comparison,
                                patch: tokio::sync::Mutex::new(None),
                            }));
                        }
                        Err(reason) => omission(&mut inner.omissions, reason, 1),
                    }
                } else {
                    omission(&mut inner.omissions, OmissionKind::Deadline, 1);
                }
            }
        } else {
            omission(&mut inner.omissions, OmissionKind::MissingBaseline, 1);
        }
        let mut summary = self.owner.state.lock().expect("review end").entries[&self.id]
            .summary
            .clone();
        summary.finished_ms = Some(now());
        summary.omissions = inner.omissions.clone();
        summary.phase = if summary.omissions.is_empty() {
            Phase::Complete
        } else {
            Phase::Partial
        };
        if let Some(ready) = &ready {
            summary.changed_files =
                u32::try_from(ready.comparison.files.len()).expect("bounded changed files");
            for file in &ready.comparison.files {
                summary.added_lines = summary.added_lines.saturating_add(file.added);
                summary.removed_lines = summary.removed_lines.saturating_add(file.removed);
            }
        }
        if self.owner.publish(summary, ready, false).await.is_err() {
            self.owner
                .state
                .lock()
                .expect("uncertain review receipt")
                .entries
                .get_mut(&self.id)
                .unwrap()
                .active = false;
        }
    }
    async fn retained(
        &self,
        evidence: Option<ExecutionObservationEnd>,
        cancellation: CancellationToken,
    ) {
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
                    if let Some(evidence) = evidence {
                        interval.end_work(evidence, &worker_stop).await;
                    } else {
                        interval.begin_work(&worker_stop).await;
                    }
                }))
        };
        tokio::select! {biased;()=cancellation.cancelled()=>{},()=self.owner.stop.cancelled()=>{},_=task=>{}}
    }
}
#[async_trait]
impl ExecutionInterval for Interval {
    async fn begin(&self, cancellation: CancellationToken) {
        self.retained(None, cancellation).await;
    }
    async fn end(&self, evidence: ExecutionObservationEnd, cancellation: CancellationToken) {
        self.retained(Some(evidence), cancellation).await;
    }
}

#[cfg(all(test, target_os = "linux"))]
#[path = "owner_tests.rs"]
mod tests;
