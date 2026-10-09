//! Rebuildable product history with source-owned authorization and exact captures.
#![forbid(unsafe_code)]
#![warn(missing_docs)]
#![allow(clippy::missing_errors_doc)]
mod authority;
mod cache;
mod discovery;
mod plugin;
mod source;
mod tools;
pub use authority::HistoryAuthority;
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
fn session_error(error: rsi_session_protocol::SessionError) -> ApiError {
    match error {
        rsi_session_protocol::SessionError::Api(error) => error,
        rsi_session_protocol::SessionError::ShuttingDown => ApiError::ShuttingDown,
        rsi_session_protocol::SessionError::Capacity => ApiError::Capacity,
        _ => ApiError::Unavailable,
    }
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
    ingress: Arc<dyn rsi_session_protocol::SessionIngress>,
    external: Arc<dyn ExternalConversations>,
    workspaces: Arc<dyn WorkspaceRegistry>,
    references: Arc<References>,
    resolver: Arc<dyn rsi_execution::ExecutionResolver>,
    cache: Arc<cache::Cache>,
    execution: Execution,
    tasks: TaskTracker,
    slots: Arc<Semaphore>,
    writer: Arc<Semaphore>,
    stop: CancellationToken,
    admission: Mutex<()>,
    protection: Option<Arc<dyn rsi_session_protocol::SessionProtection>>,
    discovery: Mutex<std::collections::VecDeque<discovery::Pass>>,
}
/// Exact source and authorization providers retained by one history owner.
#[derive(Debug)]
pub struct HistorySources {
    /// Durable native source.
    pub store: Arc<dyn SessionStore>,
    /// Current Session identity and draft lifetime provider.
    pub sessions: Arc<dyn rsi_session_protocol::SessionService>,
    /// Actual human-origin Session narrowing, including live drafts.
    pub ingress: Arc<dyn rsi_session_protocol::SessionIngress>,
    /// External observed conversation owner.
    pub external: Arc<dyn ExternalConversations>,
    /// Registered coordinate authority.
    pub workspaces: Arc<dyn WorkspaceRegistry>,
    /// Exact reference capture owner.
    pub references: Arc<References>,
    /// Current execution-location admission, including offline metadata access.
    pub resolver: Arc<dyn rsi_execution::ExecutionResolver>,
    /// Product protection policy; absent policies fail closed for protected sources.
    pub protection: Option<Arc<dyn rsi_session_protocol::SessionProtection>>,
}
impl ProductHistorySearch {
    /// Opens the independently leased, rebuildable cache before publishing a service.
    pub async fn open(
        directory: PathBuf,
        sources: HistorySources,
        execution: Execution,
    ) -> Result<Arc<Self>> {
        let cache = cache::Cache::open(directory).await?;
        Ok(Arc::new(Self {
            store: sources.store,
            sessions: sources.sessions,
            ingress: sources.ingress,
            external: sources.external,
            workspaces: sources.workspaces,
            references: sources.references,
            resolver: sources.resolver,
            cache: Arc::new(cache),
            execution,
            tasks: TaskTracker::new(),
            slots: Arc::new(Semaphore::new(2)),
            writer: Arc::new(Semaphore::new(1)),
            stop: CancellationToken::new(),
            admission: Mutex::new(()),
            protection: sources.protection,
            discovery: Mutex::new(std::collections::VecDeque::new()),
        }))
    }
    /// Executes one finite request. A dropped waiter never releases a dispatched worker.
    ///
    /// # Panics
    /// Panics if an earlier panic poisoned admission state.
    pub async fn call(
        self: &Arc<Self>,
        authority: HistoryAuthority,
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
                let result = owner.execute(authority, request, &worker_stop).await;
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
    #[expect(
        clippy::too_many_lines,
        reason = "One exhaustive owner dispatch retains authorization and settlement for every History operation."
    )]
    async fn execute(
        &self,
        authority: HistoryAuthority,
        request: Request,
        stop: &CancellationToken,
    ) -> Result<Reply> {
        if matches!(
            request,
            Request::Discover { .. }
                | Request::Query { .. }
                | Request::Progress { .. }
                | Request::Reset { .. }
        ) {
            return self.execute_query(authority, request, stop).await;
        }
        let scope = request
            .scope()
            .ok_or_else(|| invalid("missing source"))?
            .clone();
        let source = self.authorize(&authority, &scope, stop).await?;
        let protection = source.protection.clone();
        let coordinates = source.coordinates.clone();
        if protection.is_cancelled() {
            return Err(ApiError::Unauthorized);
        }
        let key = cache::key(&scope);
        let reply = match &request {
            Request::Advance { .. } => {
                let _writer = self.writer.try_acquire().map_err(|_| ApiError::Capacity)?;
                self.cache.remember(scope.clone(), stop.clone()).await?;
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
            } => {
                self.freeze(&authority, source, hit, target, *start, *end, stop)
                    .await?
            }
            Request::Discover { .. }
            | Request::Query { .. }
            | Request::Progress { .. }
            | Request::Reset { .. } => unreachable!("aggregate dispatch handled above"),
        };
        publish_source_reply(
            &authority,
            self.resolver.as_ref(),
            &coordinates,
            &protection,
            &request,
            reply,
        )
    }
    #[expect(
        clippy::too_many_arguments,
        reason = "Capture independently binds authority, source/hit, actual target and byte selection."
    )]
    async fn freeze(
        &self,
        authority: &HistoryAuthority,
        source: source::Source,
        hit: &Hit,
        target: &rsi_agent_session_protocol::SessionId,
        start: usize,
        end: usize,
        stop: &CancellationToken,
    ) -> Result<Reply> {
        // The history location permit covers ordinary Sessions at these coordinates.
        // A hit never authorizes a protected receiving Header.
        let target_handle = match authority {
            HistoryAuthority::Caller(origin) => {
                self.ingress.scoped(origin.clone()).attach(target).await
            }
            HistoryAuthority::Agent(caller) if caller.session_id() == target => {
                self.sessions.attach(target).await
            }
            HistoryAuthority::Agent(_) => return Err(ApiError::Unauthorized),
        }
        .map_err(session_error)?;
        check(stop)?;
        let target = target_handle.header().await.map_err(session_error)?;
        if source.protected || target.protection().is_some() {
            return Err(ApiError::Unauthorized);
        }
        check(stop)?;
        let target_admission = authority.admit(self.resolver.as_ref(), target.coordinates())?;
        let original = self.original(&source, hit, stop).await?;
        let mut selection = hit.original.clone();
        selection.start = start;
        selection.end = end;
        let source_identity = source.identity.clone();
        let source_coordinates = source.coordinates.clone();
        let target_coordinates = target.coordinates().clone();
        let resolver = self.resolver.clone();
        let gate_authority = authority.clone();
        let gate_source = source_coordinates.clone();
        let gate_target = target_coordinates.clone();
        let source_protection = source.protection.clone();
        let context = rsi_agent_references::CaptureContext::new(
            source_identity,
            source.coordinates.clone(),
            target,
            (source, target_admission, target_handle),
            move || {
                !source_protection.is_cancelled()
                    && gate_authority
                        .admit(resolver.as_ref(), &gate_source)
                        .is_ok()
                    && gate_authority
                        .admit(resolver.as_ref(), &gate_target)
                        .is_ok()
            },
        )
        .map_err(invalid)?;
        let reference = match context.source().clone() {
            rsi_agent_session_protocol::ReferenceSource::Native { binding } => {
                self.references
                    .capture_selected(binding, context, selection, stop.clone())
                    .await
            }
            observed @ rsi_agent_session_protocol::ReferenceSource::Observed { .. } => {
                self.references
                    .capture_observed(
                        rsi_agent_references::ObservedReferenceText {
                            source: observed,
                            coordinates: context.coordinates().clone(),
                            text: original.text,
                            selection,
                        },
                        context,
                        stop.clone(),
                    )
                    .await
            }
        };
        let _source_check = authority.admit(self.resolver.as_ref(), &source_coordinates)?;
        let _target_check = authority.admit(self.resolver.as_ref(), &target_coordinates)?;
        let reference = reference.map_err(invalid)?;
        Ok(Reply::Frozen { reference })
    }
}

fn publish_source_reply(
    authority: &HistoryAuthority,
    resolver: &dyn rsi_execution::ExecutionResolver,
    coordinates: &rsi_execution::ExecutionCoordinates,
    protection: &CancellationToken,
    request: &Request,
    reply: Reply,
) -> Result<Reply> {
    let _fresh = authority.admit(resolver, coordinates)?;
    if protection.is_cancelled() {
        return Err(ApiError::Unauthorized);
    }
    rsi_history_api::validate_reply(request, &reply)?;
    Ok(reply)
}

#[cfg(test)]
mod error_tests {
    use super::*;
    #[test]
    fn freeze_session_refusals_preserve_the_history_api_categories() {
        use rsi_session_protocol::SessionError;
        assert_eq!(
            session_error(SessionError::Api(ApiError::Unauthorized)),
            ApiError::Unauthorized
        );
        assert_eq!(
            session_error(SessionError::NotFound("target".into())),
            ApiError::Unavailable
        );
        assert_eq!(
            session_error(SessionError::Backend("read failed".into())),
            ApiError::Unavailable
        );
        assert_eq!(session_error(SessionError::Capacity), ApiError::Capacity);
    }
}
