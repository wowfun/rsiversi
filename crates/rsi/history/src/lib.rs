//! Rebuildable product history with source-owned authorization and exact captures.
#![forbid(unsafe_code)]
#![warn(missing_docs)]
#![allow(clippy::missing_errors_doc)]
mod cache;
mod plugin;
mod source;
mod tools;
pub use plugin::{HistoryApiFactory, HistoryContract, HistoryFactory};
use rsi_acp_protocol::service::ExternalConversations;
use rsi_agent_references::References;
use rsi_agent_store_protocol::SessionStore;
use rsi_api_protocol::{ApiError, Result};
use rsi_history_api::{Coverage, Hit, Reply, Request, Scope};
use rsi_meta::Execution;
use rsi_workspace_protocol::WorkspaceRegistry;
use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
};
use tokio::sync::Semaphore;
use tokio_util::{sync::CancellationToken, task::TaskTracker};
pub use tools::HistoryToolsFactory;
fn invalid(e: impl std::fmt::Display) -> ApiError {
    ApiError::Invalid(e.to_string())
}
fn check(stop: &CancellationToken) -> Result<()> {
    if stop.is_cancelled() {
        Err(ApiError::ShuttingDown)
    } else {
        Ok(())
    }
}

/// Dedicated product cache owner, with two retained finite workers.
#[derive(Debug)]
pub struct ProductHistorySearch {
    store: Arc<dyn SessionStore>,
    sessions: Arc<dyn rsi_session_protocol::SessionService>,
    external: Arc<dyn ExternalConversations>,
    workspaces: Arc<dyn WorkspaceRegistry>,
    references: Arc<References>,
    cache: Arc<cache::Cache>,
    execution: Execution,
    tasks: TaskTracker,
    slots: Arc<Semaphore>,
    writer: Arc<Semaphore>,
    stop: CancellationToken,
    admission: Mutex<()>,
}
impl ProductHistorySearch {
    /// Opens the independently leased, rebuildable cache before publishing a service.
    pub async fn open(
        directory: PathBuf,
        store: Arc<dyn SessionStore>,
        sessions: Arc<dyn rsi_session_protocol::SessionService>,
        external: Arc<dyn ExternalConversations>,
        workspaces: Arc<dyn WorkspaceRegistry>,
        references: Arc<References>,
        execution: Execution,
    ) -> Result<Arc<Self>> {
        let cache = cache::Cache::open(directory).await?;
        Ok(Arc::new(Self {
            store,
            sessions,
            external,
            workspaces,
            references,
            cache: Arc::new(cache),
            execution,
            tasks: TaskTracker::new(),
            slots: Arc::new(Semaphore::new(2)),
            writer: Arc::new(Semaphore::new(1)),
            stop: CancellationToken::new(),
            admission: Mutex::new(()),
        }))
    }
    /// Executes one finite request. A dropped waiter never releases a dispatched worker.
    ///
    /// # Panics
    /// Panics if an earlier panic poisoned admission state.
    pub async fn call(
        self: &Arc<Self>,
        request: Request,
        cancellation: CancellationToken,
    ) -> Result<Reply> {
        request.validate()?;
        let stop = self.stop.child_token();
        let _guard = stop.clone().drop_guard();
        let worker_stop = stop.clone();
        let task = {
            let _lock = self.admission.lock().expect("history admission");
            check(&self.stop)?;
            check(&cancellation)?;
            let permit = self
                .slots
                .clone()
                .try_acquire_owned()
                .map_err(|_| ApiError::Capacity)?;
            let owner = self.clone();
            self.execution.spawn(self.tasks.track_future(async move {
                let _permit = permit;
                let result = owner.execute(request, &worker_stop).await;
                check(&worker_stop)?;
                result
            }))
        };
        let deadline = self
            .execution
            .deadline_after(std::time::Duration::from_secs(30));
        tokio::select! {biased;()=cancellation.cancelled()=>Err(ApiError::ShuttingDown),()=self.stop.cancelled()=>Err(ApiError::ShuttingDown),result=deadline.timeout(task)=>result.map_err(|_|ApiError::Unavailable)?.map_err(|_|ApiError::Unavailable)?}
    }
    /// Closes admission and drains actual reads and `SQLite` workers before releasing the lease.
    ///
    /// # Panics
    /// Panics if an earlier panic poisoned admission state.
    pub async fn close(&self) {
        {
            let _lock = self.admission.lock().expect("history admission");
            self.stop.cancel();
            self.slots.close();
            self.tasks.close();
        }
        self.tasks.wait().await;
        self.cache.close().await;
    }
    async fn execute(&self, request: Request, stop: &CancellationToken) -> Result<Reply> {
        let scope = request.scope().clone();
        let source = self.authorize(&scope, stop).await?;
        let key = cache::key(&scope);
        let reply = match &request {
            Request::Advance { .. } => {
                let _writer = self.writer.try_acquire().map_err(|_| ApiError::Capacity)?;
                let coverage = self
                    .cache
                    .coverage(key.clone(), source.identity.clone(), stop.clone())
                    .await?;
                let batch = self
                    .batch(
                        &source,
                        rsi_history_api::decimal(&coverage.indexed_through)?,
                        stop,
                    )
                    .await?;
                self.cache
                    .advance(key, coverage, batch, stop.clone())
                    .await?
            }
            Request::Rebuild { .. } => {
                let _writer = self.writer.try_acquire().map_err(|_| ApiError::Capacity)?;
                self.cache
                    .rebuild(key, source.identity, stop.clone())
                    .await?
            }
            Request::Search { query, after, .. } => {
                let mut coverage = self
                    .cache
                    .coverage(key.clone(), source.identity.clone(), stop.clone())
                    .await?;
                let (horizon, has_more) = self
                    .horizon(
                        &source,
                        rsi_history_api::decimal(&coverage.indexed_through)?,
                        stop,
                    )
                    .await?;
                coverage.observed_through = horizon.to_string();
                coverage.has_more = has_more;
                self.cache
                    .search(
                        key,
                        scope.clone(),
                        query.clone(),
                        after.clone(),
                        coverage,
                        stop.clone(),
                    )
                    .await?
            }
            Request::Read { hit, offset, .. } => {
                let original = self.original(&source, hit, stop).await?;
                let start = original.text.floor_char_boundary(*offset);
                let end = original
                    .text
                    .floor_char_boundary((start + 65536).min(original.text.len()));
                Reply::Original {
                    hit: hit.clone(),
                    offset: start,
                    next_offset: end,
                    has_more: end < original.text.len(),
                    text: original.text[start..end].into(),
                }
            }
            Request::Freeze {
                hit,
                target,
                start,
                end,
                ..
            } => self.freeze(source, hit, target, *start, *end, stop).await?,
        };
        rsi_history_api::validate_reply(&request, &reply)?;
        Ok(reply)
    }
    async fn freeze(
        &self,
        source: source::Source,
        hit: &Hit,
        target: &rsi_agent_session_protocol::SessionId,
        start: usize,
        end: usize,
        stop: &CancellationToken,
    ) -> Result<Reply> {
        // Authorize the target independently; a hit never grants a receiving Header.
        let target_handle = self.sessions.attach(target).await.map_err(invalid)?;
        check(stop)?;
        let target = target_handle.header().await.map_err(invalid)?;
        check(stop)?;
        if target.canonical_cwd() != source.cwd {
            return Err(invalid(
                "reference target is outside the requested workspace",
            ));
        }
        let original = self.original(&source, hit, stop).await?;
        let mut selection = hit.original.clone();
        selection.start = start;
        selection.end = end;
        let reference = match source.identity {
            rsi_agent_session_protocol::ReferenceSource::Native { binding } => {
                self.references
                    .capture_selected(binding, target, selection, stop.clone())
                    .await
            }
            observed @ rsi_agent_session_protocol::ReferenceSource::Observed { .. } => {
                self.references
                    .capture_observed(
                        rsi_agent_references::ObservedReferenceText {
                            source: observed,
                            canonical_cwd: source.cwd,
                            text: original.text,
                            selection,
                        },
                        target,
                        stop.clone(),
                    )
                    .await
            }
        }
        .map_err(invalid)?;
        Ok(Reply::Frozen { reference })
    }
}
