use super::{
    ApiError, CancellationToken, HistoryAuthority, ProductHistorySearch, Reply, Request, Result,
    Scope, cache, check, invalid, source,
};
use rsi_agent_session_protocol::ReferenceSource;
use rsi_conversation::ConversationIdentity;
use rsi_history_api::{DiscoveryCursor, QueryProgress, QueryScope, SourceCoverage};
use rsi_workspace_protocol::{WorkspaceCursor, WorkspaceId};
use sha2::{Digest as _, Sha256};
use std::time::{Duration, Instant};

#[derive(Clone, Debug)]
pub(super) struct Pass {
    token: String,
    cursor: DiscoveryCursor,
}
fn range_key(scope: &QueryScope) -> String {
    hex::encode(Sha256::digest(
        serde_json::to_vec(scope).expect("typed scope"),
    ))
}
fn source_scope(
    source: &ReferenceSource,
    coordinates: &rsi_execution::ExecutionCoordinates,
) -> Result<Scope> {
    let conversation = match source {
        ReferenceSource::Native { binding } => {
            ConversationIdentity::Native(binding.session_id.clone())
        }
        ReferenceSource::Observed { owner, id, .. } if owner == "acp" => {
            ConversationIdentity::External(
                rsi_acp_protocol::observation::ConversationId::new(id).map_err(invalid)?,
            )
        }
        ReferenceSource::Observed { .. } => return Err(invalid("unsupported saved source")),
    };
    Ok(Scope {
        workspace: WorkspaceId::from_coordinates(coordinates),
        conversation,
    })
}
impl ProductHistorySearch {
    async fn visible(
        &self,
        authority: &HistoryAuthority,
        range: &QueryScope,
        stop: &CancellationToken,
    ) -> Result<(Vec<source::Source>, Vec<SourceCoverage>)> {
        authority.narrow(range)?;
        let known = if let QueryScope::Conversation { source } = range {
            vec![source.clone()]
        } else {
            self.cache.known(range.clone(), stop.clone()).await?
        };
        let caller = authority.principal();
        let scope_key = range_key(range);
        let unavailable: std::collections::BTreeSet<_> = self
            .discovery
            .lock()
            .expect("history discovery")
            .iter()
            .rev()
            .find(|pass| pass.cursor.caller == caller && pass.cursor.scope_key == scope_key)
            .map(|pass| pass.cursor.unavailable.iter().cloned().collect())
            .unwrap_or_default();
        let mut owners = Vec::new();
        let mut coverage = Vec::new();
        let mut admissions = source::Admissions::new();
        for scope in known {
            check(stop)?;
            match self
                .authorize_shared(authority, &scope, stop, &mut admissions)
                .await
            {
                Ok(source) => {
                    let mut progress = self
                        .cache
                        .coverage(cache::key(&scope), source.identity.clone(), stop.clone())
                        .await?;
                    let source_unavailable = match self
                        .horizon(
                            &source,
                            rsi_history_api::decimal(&progress.indexed_through)?,
                            stop,
                        )
                        .await
                    {
                        Ok((horizon, has_more)) => {
                            progress.observed_through = horizon.to_string();
                            progress.has_more = has_more;
                            false
                        }
                        Err(
                            ApiError::Unauthorized
                            | ApiError::Unavailable
                            | ApiError::Invalid(_)
                            | ApiError::Backend(_),
                        ) => true,
                        Err(error) => return Err(error),
                    };
                    let label = format!(
                        "{:?}:{}",
                        source.coordinates.location(),
                        source.coordinates.path()
                    );
                    let label = label[..label.floor_char_boundary(label.len().min(512))].to_owned();
                    let unavailable =
                        source_unavailable || unavailable.contains(&cache::key(&scope));
                    coverage.push(SourceCoverage {
                        label,
                        unavailable,
                        scope,
                        coverage: progress,
                        reference_allowed: !source.protected,
                    });
                    owners.push(source);
                }
                Err(
                    ApiError::Unauthorized
                    | ApiError::Unavailable
                    | ApiError::Invalid(_)
                    | ApiError::Backend(_),
                ) => {}
                Err(error) => return Err(error),
            }
        }
        coverage.sort_by_cached_key(|c| cache::key(&c.scope));
        Ok((owners, coverage))
    }
    fn progress(
        &self,
        authority: &HistoryAuthority,
        range: &QueryScope,
        sources: &[SourceCoverage],
    ) -> QueryProgress {
        let caller = authority.principal();
        let key = range_key(range);
        let guard = self.discovery.lock().expect("history discovery");
        let pass = guard
            .iter()
            .rev()
            .find(|p| p.cursor.caller == caller && p.cursor.scope_key == key);
        QueryProgress {
            discovery_complete: pass.is_some_and(|p| p.cursor.complete()),
            capacity_limited: pass.is_some_and(|p| p.cursor.capacity_limited),
            metadata_unavailable: pass.is_some_and(|p| p.cursor.metadata_unavailable),
            visible_sources: sources.len(),
            pending_sources: sources
                .iter()
                .filter(|s| s.coverage.has_more && !s.unavailable)
                .count(),
            continuation: pass
                .filter(|p| {
                    !p.cursor.capacity_limited
                        && ((!p.cursor.workspace_done
                            || !p.cursor.native_done
                            || !p.cursor.external_done)
                            || sources
                                .iter()
                                .any(|s| s.coverage.has_more && !s.unavailable))
                })
                .map(|p| p.token.clone()),
        }
    }
    pub(super) async fn execute_query(
        &self,
        authority: HistoryAuthority,
        request: Request,
        stop: &CancellationToken,
    ) -> Result<Reply> {
        let range = match &request {
            Request::Discover { scope, .. }
            | Request::Query { scope, .. }
            | Request::Progress { scope, .. }
            | Request::Reset { scope, .. } => scope.clone(),
            _ => unreachable!(),
        };
        authority.narrow(&range)?;
        if let Request::Discover { after, .. } = &request {
            self.discover(&authority, &range, after.as_deref(), stop)
                .await?;
        }
        let (owners, mut sources) = self.visible(&authority, &range, stop).await?;
        if let Request::Reset { after, .. } = &request {
            let _writer = self.writer.try_acquire().map_err(|_| ApiError::Capacity)?;
            for status in sources
                .iter_mut()
                .filter(|s| after.as_ref().is_none_or(|a| cache::key(&s.scope) > *a))
                .take(64)
            {
                check(stop)?;
                let source = self.authorize(&authority, &status.scope, stop).await?;
                let Reply::Coverage { coverage } = self
                    .cache
                    .rebuild(cache::key(&status.scope), source.identity, stop.clone())
                    .await?
                else {
                    unreachable!("cache rebuild coverage")
                };
                status.coverage = coverage;
            }
        }
        let progress = self.progress(&authority, &range, &sources);
        let reply = match &request {
            Request::Query { query, after, .. } => {
                self.cache
                    .query(
                        range,
                        query.clone(),
                        after.clone(),
                        authority.principal(),
                        sources,
                        progress,
                        stop.clone(),
                    )
                    .await?
            }
            Request::Progress { after, .. } | Request::Reset { after, .. } => {
                coverage_page(progress, sources, after.as_deref())?
            }
            Request::Discover { .. } => coverage_page(progress, sources, None)?,
            _ => unreachable!(),
        };
        authority.narrow(match &request {
            Request::Discover { scope, .. }
            | Request::Query { scope, .. }
            | Request::Progress { scope, .. }
            | Request::Reset { scope, .. } => scope,
            _ => unreachable!(),
        })?;
        for source in owners {
            if source.protection.is_cancelled() {
                return Err(ApiError::Unauthorized);
            }
            let _check = authority.admit(self.resolver.as_ref(), &source.coordinates)?;
        }
        rsi_history_api::validate_reply(&request, &reply)?;
        Ok(reply)
    }
    async fn remember_visible(
        &self,
        authority: &HistoryAuthority,
        range: &QueryScope,
        scope: Scope,
        stop: &CancellationToken,
    ) -> Result<()> {
        if !range.contains(&scope) {
            return Ok(());
        }
        match self.authorize(authority, &scope, stop).await {
            Ok(_source) => self.cache.remember(scope, stop.clone()).await,
            Err(
                ApiError::Unauthorized
                | ApiError::Unavailable
                | ApiError::Invalid(_)
                | ApiError::Backend(_),
            ) => Ok(()),
            Err(error) => Err(error),
        }
    }
    #[expect(
        clippy::too_many_lines,
        reason = "One bounded scan shares metadata quotas and fair indexing across three source families."
    )]
    async fn discover(
        &self,
        authority: &HistoryAuthority,
        range: &QueryScope,
        after: Option<&str>,
        stop: &CancellationToken,
    ) -> Result<()> {
        let _writer = self.writer.try_acquire().map_err(|_| ApiError::Capacity)?;
        let caller = authority.principal();
        let scope_key = range_key(range);
        let mut cursor = if let Some(token) = after {
            self.discovery
                .lock()
                .expect("history discovery")
                .iter()
                .find(|p| {
                    p.token == token && p.cursor.caller == caller && p.cursor.scope_key == scope_key
                })
                .map(|p| p.cursor.clone())
                .ok_or_else(|| invalid("Stale discovery continuation; refresh"))?
        } else {
            let index_after = self
                .discovery
                .lock()
                .expect("history discovery")
                .iter()
                .rev()
                .find(|p| p.cursor.caller == caller && p.cursor.scope_key == scope_key)
                .and_then(|p| p.cursor.index_after.clone());
            DiscoveryCursor {
                index_after,
                caller: caller.clone(),
                scope_key: scope_key.clone(),
                workspace_done: !matches!(range, QueryScope::AccessibleHost),
                native_done: matches!(range, QueryScope::Conversation { .. }),
                external_done: matches!(range, QueryScope::Conversation { .. }),
                ..Default::default()
            }
        };
        if cursor.capacity_limited {
            return Ok(());
        }
        let started = Instant::now();
        let mut items = 0;
        let mut bytes = 0usize;
        while items < 64
            && bytes < 2 * 1024 * 1024
            && started.elapsed() < Duration::from_secs(1)
            && !cursor.capacity_limited
        {
            check(stop)?;
            if !cursor.workspace_done {
                if cursor.workspaces == 4096 {
                    cursor.capacity_limited = true;
                    break;
                }
                let Ok(page) = self
                    .workspaces
                    .list(
                        cursor
                            .workspace_after
                            .map(|after_order| WorkspaceCursor { after_order }),
                        1,
                    )
                    .await
                else {
                    cursor.metadata_unavailable = true;
                    cursor.workspace_done = true;
                    continue;
                };
                if page.records.is_empty() {
                    cursor.workspace_done = true;
                    continue;
                }
                let record = &page.records[0];
                let charge = serde_json::to_vec(record).map_err(invalid)?.len();
                if bytes + charge > 2 * 1024 * 1024 {
                    break;
                }
                bytes += charge;
                items += 1;
                cursor.workspaces += 1;
                cursor.workspace_after = page.next.map(|p| p.after_order);
                cursor.workspace_done = page.next.is_none();
                continue;
            }
            if cursor.native_done && cursor.external_done {
                break;
            }
            if cursor.sources == 4096 {
                cursor.capacity_limited = true;
                break;
            }
            if !cursor.external_done && (cursor.external_next || cursor.native_done) {
                let Ok(page) = self.external.list(cursor.external_after.clone()).await else {
                    cursor.metadata_unavailable = true;
                    cursor.external_done = true;
                    continue;
                };
                let length = page.len();
                let mut consumed = 0;
                for snapshot in page {
                    let charge = serde_json::to_vec(&snapshot).map_err(invalid)?.len();
                    if items == 64
                        || bytes + charge > 2 * 1024 * 1024
                        || cursor.sources == 4096
                        || started.elapsed() >= Duration::from_secs(1)
                    {
                        break;
                    }
                    bytes += charge;
                    items += 1;
                    cursor.sources += 1;
                    consumed += 1;
                    cursor.external_after = Some(snapshot.id.clone());
                    let coordinates = rsi_execution::ExecutionCoordinates::new(
                        rsi_execution::ExecutionLocation::Local,
                        snapshot.cwd,
                    )
                    .map_err(invalid)?;
                    let source = ReferenceSource::Observed {
                        owner: "acp".into(),
                        id: snapshot.id.as_str().into(),
                        epoch: snapshot.epoch,
                    };
                    if let Err(error) = self
                        .remember_visible(
                            authority,
                            range,
                            source_scope(&source, &coordinates)?,
                            stop,
                        )
                        .await
                    {
                        if error == ApiError::Capacity {
                            cursor.capacity_limited = true;
                            break;
                        }
                        return Err(error);
                    }
                }
                if consumed == length && length < 64 {
                    cursor.external_done = true;
                }
                cursor.external_next = false;
            } else if !cursor.native_done {
                // Header bodies have their own 1 MiB boundary; never materialize 64 at once.
                if bytes + 1024 * 1024 > 2 * 1024 * 1024 {
                    break;
                }
                let Ok(page) = self
                    .store
                    .list_sessions(cursor.native_after.as_ref(), 1)
                    .await
                else {
                    cursor.metadata_unavailable = true;
                    cursor.native_done = true;
                    continue;
                };
                let Some(id) = page.sessions.first() else {
                    cursor.native_done = true;
                    continue;
                };
                let Ok(header) = self.store.header(id).await else {
                    cursor.metadata_unavailable = true;
                    cursor.native_after = Some(id.clone());
                    cursor.native_done = !page.has_more;
                    cursor.sources += 1;
                    items += 1;
                    cursor.external_next = true;
                    continue;
                };
                let charge = serde_json::to_vec(&header).map_err(invalid)?.len();
                if bytes + charge > 2 * 1024 * 1024 {
                    break;
                }
                bytes += charge;
                items += 1;
                cursor.sources += 1;
                cursor.native_after = Some(id.clone());
                cursor.native_done = !page.has_more;
                cursor.external_next = true;
                let source = ReferenceSource::Native {
                    binding: rsi_agent_session_protocol::ReferenceBinding {
                        session_id: id.clone(),
                        header_sha256: header.fingerprint().map_err(invalid)?,
                    },
                };
                if let Err(error) = self
                    .remember_visible(
                        authority,
                        range,
                        source_scope(&source, header.coordinates())?,
                        stop,
                    )
                    .await
                {
                    if error == ApiError::Capacity {
                        cursor.capacity_limited = true;
                        break;
                    }
                    return Err(error);
                }
            }
        }
        let (owners, sources) = self.visible(authority, range, stop).await?;
        // Rotate by the previous source key, independent of long transcript size.
        if !sources.is_empty() {
            let start = cursor
                .index_after
                .as_ref()
                .and_then(|last| sources.iter().position(|s| cache::key(&s.scope) > *last))
                .unwrap_or(0);
            for i in 0..sources.len() {
                let status = &sources[(start + i) % sources.len()];
                let source_key = cache::key(&status.scope);
                if cursor.unavailable.contains(&source_key) {
                    continue;
                }
                cursor.index_after = Some(source_key.clone());
                if let Ok(source) = self.authorize(authority, &status.scope, stop).await {
                    let previous = self
                        .cache
                        .coverage(source_key.clone(), source.identity.clone(), stop.clone())
                        .await?;
                    match self
                        .batch(
                            &source,
                            rsi_history_api::decimal(&previous.indexed_through)?,
                            stop,
                        )
                        .await
                    {
                        Ok(batch) => {
                            match self
                                .cache
                                .advance(cache::key(&status.scope), previous, batch, stop.clone())
                                .await
                            {
                                Ok(_) => {}
                                Err(ApiError::Capacity) => cursor.capacity_limited = true,
                                Err(error) => return Err(error),
                            }
                        }
                        Err(ApiError::Capacity) => cursor.capacity_limited = true,
                        Err(
                            ApiError::Unauthorized
                            | ApiError::Unavailable
                            | ApiError::Invalid(_)
                            | ApiError::Backend(_),
                        ) => {
                            if cursor.unavailable.len() == 64 {
                                cursor.capacity_limited = true;
                            } else {
                                cursor.unavailable.push(source_key);
                            }
                        }
                        Err(error) => return Err(error),
                    }
                    break;
                }
            }
        }
        for source in owners {
            if source.protection.is_cancelled() {
                return Err(ApiError::Unauthorized);
            }
            let _check = authority.admit(self.resolver.as_ref(), &source.coordinates)?;
        }
        let mut token = [0u8; 16];
        getrandom::fill(&mut token).map_err(invalid)?;
        let mut passes = self.discovery.lock().expect("history discovery");
        passes.retain(|p| p.cursor.caller != caller || p.cursor.scope_key != scope_key);
        if passes.len() == 64 {
            passes.pop_front();
        }
        passes.push_back(Pass {
            token: hex::encode(token),
            cursor,
        });
        Ok(())
    }
}

fn coverage_page(
    progress: QueryProgress,
    sources: Vec<SourceCoverage>,
    after: Option<&str>,
) -> Result<Reply> {
    let mut page = Vec::new();
    let mut retained = 32 * 1024;
    let mut more = false;
    for source in sources
        .into_iter()
        .filter(|s| after.is_none_or(|a| cache::key(&s.scope).as_str() > a))
    {
        let charge = serde_json::to_vec(&source).map_err(invalid)?.len() + 1;
        if page.len() == 64 || retained + charge > 256 * 1024 {
            more = true;
            break;
        }
        retained += charge;
        page.push(source);
    }
    let next = more.then(|| cache::key(&page.last().expect("one finite source fits").scope));
    Ok(Reply::Progress {
        progress,
        sources: page,
        next,
    })
}
