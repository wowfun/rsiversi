use super::*;
use rsi_agent_session_protocol::{AgentPresetId, FrozenAgentSettings, SessionHeader};
use rsi_session_protocol::{
    CreateSession, RecentSessionCursor, RecentSessionPage, SessionHandle, SessionSummary,
};
use rsi_workspace_protocol::{WorkspaceCursor, WorkspacePage, WorkspaceRecord, WorkspaceStatus};

#[derive(Debug)]
struct Rows(Vec<SessionSummary>);
#[async_trait]
impl SessionService for Rows {
    async fn create(
        &self,
        _: CreateSession,
    ) -> rsi_session_protocol::Result<Arc<dyn SessionHandle>> {
        panic!("navigation query cannot create")
    }
    async fn attach(&self, _: &SessionId) -> rsi_session_protocol::Result<Arc<dyn SessionHandle>> {
        panic!("navigation query cannot load transcript")
    }
    async fn list_recent(
        &self,
        after: Option<&RecentSessionCursor>,
        limit: usize,
    ) -> rsi_session_protocol::Result<RecentSessionPage> {
        assert!(limit <= 256);
        let mut matching = self.0.iter().filter(|row| {
            after.is_none_or(|after| rsi_navigation_api::cursor_advances(after, &row.cursor()))
        });
        let sessions = matching.by_ref().take(limit).cloned().collect();
        Ok(RecentSessionPage {
            sessions,
            has_more: matching.next().is_some(),
        })
    }
}
#[derive(Debug)]
struct Workspaces;
#[async_trait]
impl WorkspaceRegistry for Workspaces {
    async fn get(&self, id: &WorkspaceId) -> rsi_workspace_protocol::Result<WorkspaceRecord> {
        Err(WorkspaceError::Unknown(id.clone()))
    }
    async fn list(
        &self,
        _: Option<WorkspaceCursor>,
        _: usize,
    ) -> rsi_workspace_protocol::Result<WorkspacePage> {
        panic!("grouping uses exact identity")
    }
    async fn get_or_create(
        &self,
        _: &std::path::Path,
    ) -> rsi_workspace_protocol::Result<WorkspaceRecord> {
        panic!("navigation cannot register workspaces")
    }
    async fn status(&self, _: &WorkspaceId) -> rsi_workspace_protocol::Result<WorkspaceStatus> {
        panic!("navigation cannot probe filesystem")
    }
    async fn delete_registration(&self, _: &WorkspaceId) -> rsi_workspace_protocol::Result<bool> {
        panic!("navigation cannot delete workspaces")
    }
}
#[derive(Debug)]
struct ReadOnlyDomain(DomainSpec);
#[async_trait]
impl Domain for ReadOnlyDomain {
    fn spec(&self) -> &DomainSpec {
        &self.0
    }
    async fn snapshot(&self) -> BTreeMap<String, serde_json::Value> {
        BTreeMap::new()
    }
    async fn put(
        &self,
        _: &str,
        _: serde_json::Value,
    ) -> std::result::Result<(), rsi_storage::StorageError> {
        panic!("query cannot write metadata")
    }
    async fn delete(&self, _: &str) -> std::result::Result<bool, rsi_storage::StorageError> {
        panic!("query cannot delete metadata")
    }
}
fn owner() -> Arc<Navigation> {
    let settings = FrozenAgentSettings::new(
        "default",
        "system",
        rsi_ai_protocol::ModelRef::new("fixture", "model").unwrap(),
        rsi_sandbox::SandboxMode::WorkspaceWrite,
        false,
    )
    .unwrap();
    let rows = (1..=300)
        .rev()
        .map(|id| SessionSummary {
            header: SessionHeader::new(
                SessionId::new(format!("row-{id:03}")).unwrap(),
                id,
                "/unregistered",
                AgentPresetId::new("fixture").unwrap(),
                settings.clone(),
            )
            .unwrap(),
        })
        .collect();
    Arc::new(Navigation {
        domain: Arc::new(ReadOnlyDomain(DomainSpec {
            id: "test".into(),
            backend: "fixture".into(),
            version: 1,
            maximum_records: 1,
            maximum_bytes: 8 * 1024 * 1024,
        })),
        session: Arc::new(Rows(rows)),
        workspace: Arc::new(Workspaces),
        epoch: HostEpoch::generate().unwrap(),
        state: Mutex::new(State {
            closed: false,
            uncertain: false,
            document: Arc::new(Document::default()),
        }),
        slots: Arc::new(Semaphore::new(8)),
        writer: Arc::new(Semaphore::new(1)),
        tasks: TaskTracker::new(),
        execution: Execution::native(tokio::runtime::Handle::current()),
    })
}
#[tokio::test]
async fn empty_search_page_advances_last_scanned_position_and_fences_query_and_epochs() {
    let owner = owner();
    let filter = NavigationFilter {
        query: "row-001".into(),
        ..Default::default()
    };
    let first = owner.query(filter.clone(), None).unwrap().await.unwrap();
    assert!(first.entries.is_empty());
    assert_eq!(first.scanned, 256);
    let cursor = first.next.unwrap();
    assert_eq!(cursor.after.session_id.as_str(), "row-045");
    let last = owner
        .query(filter.clone(), Some(cursor.clone()))
        .unwrap()
        .await
        .unwrap();
    assert_eq!(last.scanned, 44);
    assert_eq!(last.entries[0].session.as_str(), "row-001");
    assert!(last.entries[0].workspace.is_none());
    assert!(last.next.is_none());
    for wrong in [
        {
            let mut c = cursor.clone();
            c.host_epoch = HostEpoch::generate().unwrap();
            c
        },
        {
            let mut c = cursor.clone();
            c.metadata_revision = "1".into();
            c
        },
        {
            let mut c = cursor.clone();
            c.filter.archived = true;
            c
        },
    ] {
        assert!(matches!(
            owner.query(filter.clone(), Some(wrong)).unwrap().await,
            Err(ApiError::Invalid(_))
        ));
    }
    owner.close().await;
    assert!(matches!(
        owner.query(filter, None),
        Err(ApiError::ShuttingDown)
    ));
}
#[tokio::test]
async fn match_limit_continues_after_64_not_after_entire_scanned_store_page() {
    let owner = owner();
    let mut after = None;
    let mut found = Vec::new();
    loop {
        let page = owner
            .query(NavigationFilter::default(), after)
            .unwrap()
            .await
            .unwrap();
        assert!(page.entries.len() <= 64);
        assert_eq!(page.scanned as usize, page.entries.len());
        found.extend(page.entries.into_iter().map(|row| row.session));
        after = page.next;
        if after.is_none() {
            break;
        }
    }
    assert_eq!(found.len(), 300);
    assert_eq!(
        found
            .into_iter()
            .collect::<std::collections::BTreeSet<_>>()
            .len(),
        300
    );
    owner.close().await;
}
#[test]
fn metadata_bounds_reject_durable_and_external_oversize_or_control_titles() {
    for title in [String::new(), "中".repeat(86), "line\nbreak".into()] {
        assert!(
            SessionMetadata {
                title: Some(title),
                archived: false
            }
            .validate()
            .is_err()
        );
    }
    let records = (0..8193)
        .map(|id| {
            (
                SessionId::new(format!("row-{id}")).unwrap(),
                SessionMetadata::default(),
            )
        })
        .collect();
    assert!(
        Document {
            revision: 1,
            records
        }
        .validate()
        .is_err()
    );
}
