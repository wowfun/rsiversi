#![allow(unused_variables)]
use super::*;
use crate::HistorySources;
use rsi_agent_session_protocol::*;
use rsi_agent_store_protocol::AppendBatch;
use rsi_agent_testkit::{MemoryStore, append_history_fixture};
use rsi_api_protocol::{ApiError, CallOrigin, Result};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
#[derive(Debug)]
struct Fixture {
    workspace: rsi_workspace_protocol::WorkspaceRecord,
    gets: AtomicUsize,
    revoke_at: AtomicUsize,
    revoked: CancellationToken,
}
mod sessions {
    use super::Fixture;
    use rsi_agent_session_protocol::{SessionHeader, SessionId};
    use rsi_session_protocol::*;
    use std::sync::Arc;
    #[async_trait::async_trait]
    impl SessionService for Fixture {
        async fn read_header(&self, session_id: &SessionId) -> Result<SessionHeader> {
            panic!("unused history fixture facet")
        }
        async fn create(&self, request: CreateSession) -> Result<Arc<dyn SessionHandle>> {
            panic!("unused history fixture facet")
        }
        async fn attach(&self, session_id: &SessionId) -> Result<Arc<dyn SessionHandle>> {
            panic!("unused history fixture facet")
        }
        async fn list_recent(
            &self,
            after: Option<&RecentSessionCursor>,
            limit: usize,
        ) -> Result<RecentSessionPage> {
            panic!("unused history fixture facet")
        }
    }
    #[async_trait::async_trait]
    impl SessionIngress for Fixture {
        fn scoped(&self, origin: rsi_api_protocol::CallOrigin) -> Arc<dyn SessionService> {
            panic!("unused history fixture facet")
        }
        async fn create_from(
            &self,
            request: CreateSession,
            origin: rsi_api_protocol::CallOrigin,
        ) -> Result<Arc<dyn SessionHandle>> {
            panic!("unused history fixture facet")
        }
    }
}
mod external {
    use super::Fixture;
    use rsi_acp_protocol::{
        observation::{ConversationId, Page, Snapshot},
        service::*,
    };

    #[async_trait::async_trait]
    impl ExternalConversations for Fixture {
        async fn endpoints(&self) -> Result<Vec<Endpoint>> {
            panic!("unused history fixture facet")
        }
        async fn residents(&self) -> Result<Vec<Resident>> {
            panic!("unused history fixture facet")
        }
        async fn list(&self, after: Option<ConversationId>) -> Result<Vec<Snapshot>> {
            Ok(Vec::new())
        }
        async fn view(&self, id: &ConversationId) -> Result<View> {
            panic!("unused history fixture facet")
        }
        async fn start(&self, id: ConversationId, endpoint: &str) -> Result<Snapshot> {
            panic!("unused history fixture facet")
        }
        async fn reconnect(&self, id: &ConversationId, setup: Setup) -> Result<Snapshot> {
            panic!("unused history fixture facet")
        }
        async fn submit(&self, id: &ConversationId, text: &str) -> Result<Snapshot> {
            panic!("unused history fixture facet")
        }
        async fn cancel(&self, id: &ConversationId) -> Result<Snapshot> {
            panic!("unused history fixture facet")
        }
        async fn close(&self, id: &ConversationId) -> Result<Snapshot> {
            panic!("unused history fixture facet")
        }
        async fn answer(
            &self,
            id: &ConversationId,
            generation: u64,
            permission: &str,
            option: &str,
        ) -> Result<()> {
            panic!("unused history fixture facet")
        }
        async fn page(&self, id: &ConversationId, epoch: u64, after: u64) -> Result<Page> {
            panic!("unused history fixture facet")
        }
        async fn window(
            &self,
            id: &ConversationId,
            epoch: u64,
            sequence: u64,
            start: usize,
        ) -> Result<Vec<u8>> {
            panic!("unused history fixture facet")
        }
    }
}
mod workspaces {
    use super::Fixture;
    use rsi_workspace_protocol::*;
    use std::path::Path;
    use std::sync::atomic::Ordering;
    #[async_trait::async_trait]
    impl WorkspaceRegistry for Fixture {
        async fn get(&self, id: &WorkspaceId) -> Result<WorkspaceRecord> {
            let n = self.gets.fetch_add(1, Ordering::SeqCst) + 1;
            if n == self.revoke_at.load(Ordering::SeqCst) {
                self.revoked.cancel();
            }
            assert_eq!(id, &self.workspace.id);
            Ok(self.workspace.clone())
        }
        async fn list(
            &self,
            after: Option<WorkspaceCursor>,
            limit: usize,
        ) -> Result<WorkspacePage> {
            panic!("unused history fixture facet")
        }
        async fn order_seed(&self) -> Result<WorkspaceOrderSeed> {
            panic!("unused history fixture facet")
        }
        async fn register_at(
            &self,
            location: &ExecutionLocation,
            path: &Path,
        ) -> Result<WorkspaceRecord> {
            panic!("unused history fixture facet")
        }
        async fn status(&self, id: &WorkspaceId) -> Result<WorkspaceStatus> {
            panic!("unused history fixture facet")
        }
        async fn rename(&self, id: &WorkspaceId, name: WorkspaceName) -> Result<WorkspaceRecord> {
            panic!("unused history fixture facet")
        }
        async fn delete_registration(&self, id: &WorkspaceId) -> Result<bool> {
            panic!("unused history fixture facet")
        }
    }
}
impl rsi_execution::ExecutionResolver for Fixture {
    fn visibility(&self, _: &CallOrigin) -> Result<rsi_execution::ExecutionVisibility> {
        panic!("unused visibility")
    }
    fn admit(
        &self,
        _: &CallOrigin,
        _: &rsi_execution::ExecutionLocation,
    ) -> Result<rsi_execution::ExecutionOperation> {
        if self.revoked.is_cancelled() {
            Err(ApiError::Unauthorized)
        } else {
            Ok(rsi_execution::ExecutionOperation::new(()))
        }
    }
    fn lease(
        &self,
        _: CallOrigin,
        _: &rsi_execution::ExecutionLocation,
    ) -> Result<rsi_execution::ExecutionLease> {
        panic!("offline history opened a connection")
    }
}
#[tokio::test]
async fn large_catalog_reads_horizons_once_and_only_indexes_one_source_with_final_reauthorization()
{
    catalog_case(CatalogCase::Pending).await;
}

#[tokio::test]
async fn completed_sources_do_not_consume_the_only_indexing_batch() {
    catalog_case(CatalogCase::CompletePrefix).await;
}

#[tokio::test]
async fn accessible_host_discovers_without_enumerating_the_workspace_registry() {
    catalog_case(CatalogCase::AccessibleHost).await;
}

#[tokio::test]
async fn fresh_discovery_retries_a_repaired_source_in_its_first_pass() {
    catalog_case(CatalogCase::Repaired).await;
}

enum CatalogCase {
    Pending,
    CompletePrefix,
    AccessibleHost,
    Repaired,
}

#[expect(
    clippy::too_many_lines,
    reason = "shared catalog fixture measures scheduling and revocation at public query boundaries"
)]
async fn catalog_case(case: CatalogCase) {
    let complete_prefix = matches!(case, CatalogCase::CompletePrefix | CatalogCase::Repaired);
    let accessible_host = matches!(case, CatalogCase::AccessibleHost);
    let repaired = matches!(case, CatalogCase::Repaired);
    let runtime = rsi_meta::Runtime::default();
    let directory = tempfile::tempdir().unwrap();
    let store = Arc::new(MemoryStore::new());
    let coordinates = rsi_execution::ExecutionCoordinates::new(
        rsi_execution::ExecutionLocation::Local,
        "/workspace",
    )
    .unwrap();
    let fixture = Arc::new(Fixture {
        workspace: rsi_workspace_protocol::WorkspaceRecord::new(coordinates.clone()),
        gets: AtomicUsize::new(0),
        revoke_at: AtomicUsize::new(0),
        revoked: CancellationToken::new(),
    });
    let references = Arc::new(rsi_agent_references::References::new(
        store.clone(),
        runtime.execution().clone(),
    ));
    let owner = ProductHistorySearch::open(
        directory.path().join("cache"),
        HistorySources {
            store: store.clone(),
            sessions: fixture.clone(),
            ingress: fixture.clone(),
            external: fixture.clone(),
            workspaces: fixture.clone(),
            references: references.clone(),
            resolver: fixture.clone(),
            protection: None,
        },
        runtime.execution().clone(),
    )
    .await
    .unwrap();
    let stop = CancellationToken::new();
    for n in 0..300 {
        let header = SessionHeader::new(
            SessionId::new(format!("source-{n}")).unwrap(),
            1,
            coordinates.clone(),
            AgentPresetId::new("fixture").unwrap(),
            FrozenAgentSettings::new(
                "fixture",
                "",
                rsi_ai_protocol::ModelRef::new("fixture", "model").unwrap(),
                rsi_sandbox::SandboxMode::ReadOnly,
                false,
            )
            .unwrap(),
        )
        .unwrap();
        let fact = Arc::new(
            SessionFact::new(
                1,
                1,
                SessionFactBody::TurnAccepted {
                    turn_id: TurnId::new("turn").unwrap(),
                    text: "searchable evidence".into(),
                    model: None,
                    reasoning_effort: None,
                    sandbox: rsi_sandbox::SandboxMode::ReadOnly,
                    require_approval: false,
                },
            )
            .unwrap(),
        );
        append_history_fixture(
            store.as_ref(),
            AppendBatch {
                session_id: header.session_id().clone(),
                expected_seq: 0,
                header: Some(header.clone()),
                facts: vec![fact],
            },
        )
        .await
        .unwrap();
        owner
            .cache
            .remember(
                Scope {
                    workspace: fixture.workspace.id.clone(),
                    conversation: ConversationIdentity::Native(header.session_id().clone()),
                },
                stop.clone(),
            )
            .await
            .unwrap();
    }
    let range = if accessible_host {
        QueryScope::AccessibleHost
    } else {
        QueryScope::Workspace {
            workspace: fixture.workspace.id.clone(),
        }
    };
    let authority = HistoryAuthority::Caller(CallOrigin::Local);
    let mut previous_failures = Vec::new();
    if complete_prefix {
        let (_, sources) = owner.visible(&authority, &range, &[], &stop).await.unwrap();
        if repaired {
            previous_failures.push(cache::key(&sources.last().unwrap().scope));
        }
        for status in sources.iter().take(299) {
            let source = owner
                .authorize(&authority, &status.scope, &stop)
                .await
                .unwrap();
            let previous = owner
                .cache
                .coverage(
                    cache::key(&status.scope),
                    source.identity.clone(),
                    stop.clone(),
                )
                .await
                .unwrap();
            let batch = owner.batch(&source, 0, &stop).await.unwrap();
            owner
                .cache
                .advance(cache::key(&status.scope), previous, batch, stop.clone())
                .await
                .unwrap();
        }
        fixture.gets.store(0, Ordering::SeqCst);
    }
    let token = "a".repeat(32);
    owner.discovery.lock().unwrap().push_back(Pass {
        token: token.clone(),
        cursor: DiscoveryCursor {
            caller: authority.principal(),
            scope_key: range_key(&range),
            native_done: true,
            external_done: true,
            unavailable: previous_failures,
            ..Default::default()
        },
    });
    if repaired {
        let before = store.history_read_counts();
        let Reply::Progress { progress, .. } = owner
            .execute_query(
                authority.clone(),
                Request::Discover {
                    scope: range.clone(),
                    after: Some(token.clone()),
                },
                &stop,
            )
            .await
            .unwrap()
        else {
            panic!("discovery progress");
        };
        assert_eq!(progress.pending_sources, 0);
        let after = store.history_read_counts();
        assert_eq!(
            (after.1, after.2),
            (before.1, before.2),
            "continuation retains pass failures"
        );
    }
    let before = store.history_read_counts();
    let reply = owner
        .execute_query(
            authority.clone(),
            Request::Discover {
                scope: range.clone(),
                after: if accessible_host || repaired {
                    None
                } else {
                    Some(token)
                },
            },
            &stop,
        )
        .await
        .unwrap();
    assert!(
        matches!(reply, Reply::Progress { progress, .. } if progress.visible_sources == 300 && progress.pending_sources == if complete_prefix { 0 } else { 299 })
    );
    let after = store.history_read_counts();
    assert_eq!(
        (after.0 - before.0, after.1 - before.1, after.2 - before.2),
        (300, 1, 1)
    );
    if complete_prefix || accessible_host {
        owner.close().await;
        references.close().await;
        assert!(runtime.shutdown().await.is_clean());
        return;
    }
    assert_eq!(
        fixture.gets.swap(0, Ordering::SeqCst),
        301,
        "one catalog authorization plus one indexing authorization"
    );
    let before = after;
    owner
        .execute_query(
            authority.clone(),
            Request::Progress {
                scope: range.clone(),
                after: None,
            },
            &stop,
        )
        .await
        .unwrap();
    let after = store.history_read_counts();
    assert_eq!(
        (after.0 - before.0, after.1 - before.1, after.2 - before.2),
        (300, 0, 0)
    );
    fixture.gets.store(0, Ordering::SeqCst);
    fixture.revoke_at.store(301, Ordering::SeqCst);
    let continuation = owner
        .discovery
        .lock()
        .unwrap()
        .back()
        .unwrap()
        .token
        .clone();
    assert!(matches!(
        owner
            .execute_query(
                authority,
                Request::Discover {
                    scope: range,
                    after: Some(continuation)
                },
                &stop
            )
            .await,
        Err(ApiError::Unauthorized)
    ));
    owner.close().await;
    references.close().await;
    assert!(runtime.shutdown().await.is_clean());
}
