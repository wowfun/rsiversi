use super::*;
use rsi_agent_session_protocol::{
    AgentPresetId, FrozenAgentSettings, SessionFact, SessionFactBody, SessionHeader, TurnId,
};
use rsi_agent_store_protocol::AppendBatch;
use rsi_session_protocol::{
    CreateSession, RecentSessionCursor, RecentSessionPage, SessionHandle, SessionSummary,
};
use rsi_workspace_protocol::{WorkspaceCursor, WorkspacePage, WorkspaceRecord, WorkspaceStatus};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn finite_read_tracking_can_be_polled_without_an_entered_tokio_runtime() {
    let owner = owner().await;
    let executor = tokio::runtime::Handle::current();
    std::thread::spawn(move || {
        assert!(tokio::runtime::Handle::try_current().is_err());
        let mut context = std::task::Context::from_waker(std::task::Waker::noop());
        let mut read = owner.run_read(|_| Box::pin(async { Ok(42) })).unwrap();
        match read.as_mut().poll(&mut context) {
            std::task::Poll::Ready(value) => assert_eq!(value.unwrap(), 42),
            std::task::Poll::Pending => assert_eq!(executor.block_on(read).unwrap(), 42),
        }
        let mut pending = owner
            .run_read(|_| Box::pin(std::future::pending::<Result<()>>()))
            .unwrap();
        assert!(pending.as_mut().poll(&mut context).is_pending());
        assert_eq!(owner.admission.slots.available_permits(), 7);
        drop(pending);
        executor.block_on(owner.close());
        assert_eq!(owner.admission.slots.available_permits(), 8);
        assert!(owner.admission.tasks.is_empty());
    })
    .join()
    .unwrap();
}

#[tokio::test]
async fn read_abandonment_and_retirement_release_slots_without_dispatching_more_work() {
    let owner = owner().await;
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let work = {
        let calls = calls.clone();
        move |_: Arc<Navigation>| -> BoxFuture<'static, Result<()>> {
            Box::pin(async move {
                calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                std::future::pending::<()>().await;
                Ok(())
            })
        }
    };
    let mut read = owner.run_read(work.clone()).unwrap();
    assert!(futures_util::poll!(&mut read).is_pending());
    assert_eq!(owner.admission.slots.available_permits(), 7);
    tokio::time::timeout(std::time::Duration::from_secs(1), async {
        while calls.load(std::sync::atomic::Ordering::SeqCst) != 1 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    drop(read);
    tokio::time::timeout(std::time::Duration::from_secs(1), async {
        while !owner.admission.tasks.is_empty() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(owner.admission.slots.available_permits(), 8);
    assert!(owner.admission.tasks.is_empty());
    let mut unpolled = owner.run_read(work.clone()).unwrap();
    let mut active = owner.run_read(work).unwrap();
    assert!(futures_util::poll!(&mut active).is_pending());
    tokio::time::timeout(std::time::Duration::from_secs(1), async {
        while calls.load(std::sync::atomic::Ordering::SeqCst) != 2 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 2);
    let mut retiring = Box::pin(owner.close());
    assert!(futures_util::poll!(&mut retiring).is_pending());
    retiring.await;
    assert_eq!(active.await, Err(ApiError::ShuttingDown));
    assert!(matches!(
        futures_util::poll!(&mut unpolled),
        std::task::Poll::Ready(Err(ApiError::ShuttingDown))
    ));
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 2);
    assert_eq!(owner.admission.slots.available_permits(), 8);
    assert!(owner.admission.tasks.is_empty());
}

#[derive(Debug)]
struct Rows(Vec<SessionSummary>, Arc<ReadConcurrency>);
#[derive(Debug, Default)]
struct ReadConcurrency {
    active: std::sync::atomic::AtomicUsize,
    peak: std::sync::atomic::AtomicUsize,
}
#[async_trait]
impl SessionService for Rows {
    async fn read_header(&self, id: &SessionId) -> rsi_session_protocol::Result<SessionHeader> {
        use std::sync::atomic::Ordering::SeqCst;
        let active = self.1.active.fetch_add(1, SeqCst) + 1;
        self.1.peak.fetch_max(active, SeqCst);
        tokio::task::yield_now().await;
        self.1.active.fetch_sub(1, SeqCst);
        self.0
            .iter()
            .find(|row| row.header.session_id() == id)
            .map(|row| row.header.clone())
            .ok_or_else(|| SessionError::NotFound(id.to_string()))
    }
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
        let _ = (after, limit);
        panic!("indexed navigation cannot read Header listings")
    }
}
#[derive(Debug, Default)]
struct Workspaces {
    reads: ReadConcurrency,
    calls: std::sync::atomic::AtomicUsize,
    records: BTreeMap<WorkspaceId, WorkspaceRecord>,
}
#[async_trait]
impl WorkspaceRegistry for Workspaces {
    async fn order_seed(
        &self,
    ) -> rsi_workspace_protocol::Result<rsi_workspace_protocol::WorkspaceOrderSeed> {
        unreachable!("workspace order membership is not used by this fixture")
    }
    async fn get(&self, id: &WorkspaceId) -> rsi_workspace_protocol::Result<WorkspaceRecord> {
        use std::sync::atomic::Ordering::SeqCst;
        self.calls.fetch_add(1, SeqCst);
        let active = self.reads.active.fetch_add(1, SeqCst) + 1;
        self.reads.peak.fetch_max(active, SeqCst);
        tokio::task::yield_now().await;
        self.reads.active.fetch_sub(1, SeqCst);
        self.records
            .get(id)
            .cloned()
            .ok_or_else(|| WorkspaceError::Unknown(id.clone()))
    }
    async fn list(
        &self,
        _: Option<WorkspaceCursor>,
        _: usize,
    ) -> rsi_workspace_protocol::Result<WorkspacePage> {
        panic!("grouping uses exact identity")
    }
    async fn register_at(
        &self,
        _location: &rsi_workspace_protocol::ExecutionLocation,
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
    fn ensure_available(&self) -> std::result::Result<(), rsi_storage::StorageError> {
        Ok(())
    }
    fn spec(&self) -> &DomainSpec {
        &self.0
    }
    async fn snapshot(
        &self,
    ) -> std::result::Result<BTreeMap<String, serde_json::Value>, rsi_storage::StorageError> {
        Ok(BTreeMap::new())
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
async fn owner() -> Arc<Navigation> {
    owner_with_reads(Arc::new(ReadConcurrency::default())).await
}
async fn owner_with_reads(reads: Arc<ReadConcurrency>) -> Arc<Navigation> {
    let settings = FrozenAgentSettings::new(
        "default",
        "system",
        rsi_ai_protocol::ModelRef::new("fixture", "model").unwrap(),
        rsi_sandbox::SandboxMode::WorkspaceWrite,
        false,
    )
    .unwrap();
    let rows: Vec<_> = (1..=300)
        .rev()
        .map(|id| SessionSummary {
            header: SessionHeader::new_local(
                SessionId::new(format!("row-{id:03}")).unwrap(),
                id,
                "/unregistered",
                AgentPresetId::new("fixture").unwrap(),
                settings.clone(),
            )
            .unwrap(),
        })
        .collect();
    let store = Arc::new(rsi_agent_testkit::MemoryStore::new());
    for row in &rows {
        store
            .append(AppendBatch {
                session_id: row.header.session_id().clone(),
                expected_seq: 0,
                header: Some(row.header.clone()),
                facts: vec![Arc::new(
                    SessionFact::new(
                        1,
                        row.header.created_at_ms(),
                        SessionFactBody::TurnAccepted {
                            turn_id: TurnId::new("turn").unwrap(),
                            text: "input".into(),
                            model: None,
                            reasoning_effort: None,
                            sandbox: rsi_sandbox::SandboxMode::WorkspaceWrite,
                            require_approval: false,
                        },
                    )
                    .unwrap(),
                )],
            })
            .await
            .unwrap();
    }
    Arc::new(Navigation {
        domain: Arc::new(ReadOnlyDomain(DomainSpec {
            id: "test".into(),
            backend: "fixture".into(),
            version: 1,
            maximum_records: 1,
            maximum_bytes: 8 * 1024 * 1024,
        })),
        session: Arc::new(Rows(rows, reads)),
        resolver: Arc::new(Visibility),
        store,
        protection: None,
        cursors: Mutex::new(CursorBook::default()),
        workspace: Arc::new(Workspaces::default()),
        epoch: HostEpoch::generate().unwrap(),
        state: Mutex::new(State {
            document: Arc::new(Document::default()),
        }),
        writer: Arc::new(Semaphore::new(1)),
        admission: Admission::new(
            8,
            rsi_meta::Execution::native(tokio::runtime::Handle::current()),
        ),
    })
}
#[tokio::test]
async fn grouping_queries_each_coordinate_once_per_page_pins_and_summary_batch() {
    use std::sync::atomic::Ordering::SeqCst;
    let mut owner = owner().await;
    let workspaces = Arc::new(Workspaces::default());
    Arc::get_mut(&mut owner).unwrap().workspace = workspaces.clone();
    let page = owner
        .query(&CallOrigin::Local, NavigationFilter::default(), None)
        .unwrap()
        .await
        .unwrap();
    assert_eq!(page.entries.len(), 64);
    assert_eq!(workspaces.calls.swap(0, SeqCst), 1);
    let sessions = page.entries[..3]
        .iter()
        .map(|row| row.session.clone())
        .collect::<Vec<_>>();
    owner.state.lock().unwrap().document = Arc::new(Document {
        records: sessions
            .iter()
            .map(|id| {
                (
                    id.clone(),
                    SessionMetadata {
                        pinned: true,
                        ..Default::default()
                    },
                )
            })
            .collect(),
        ..Default::default()
    });
    let pins = owner
        .pinned(CallOrigin::Local, NavigationFilter::default())
        .unwrap()
        .await
        .unwrap();
    assert_eq!(pins.entries.len(), 3);
    assert_eq!(workspaces.calls.swap(0, SeqCst), 1);
    let page = owner
        .summaries(
            &CallOrigin::Local,
            rsi_navigation_api::SummaryRequest {
                sessions: sessions.clone(),
                metadata_revision: "0".into(),
            },
        )
        .unwrap()
        .await
        .unwrap();
    assert_eq!(
        page.entries
            .iter()
            .map(|row| row.as_ref().unwrap().session.clone())
            .collect::<Vec<_>>(),
        sessions
    );
    assert_eq!(workspaces.calls.load(SeqCst), 1);
}
#[tokio::test]
async fn workspace_lookup_prefetch_is_bounded_ordered_shared_and_lazy() {
    use futures_util::TryStreamExt;
    use std::sync::atomic::Ordering::SeqCst;
    let mut owner = owner().await;
    let coordinates = (0..12)
        .map(|i| {
            ExecutionCoordinates::new(
                rsi_agent_session_protocol::ExecutionLocation::Local,
                format!("/workspace-{i}"),
            )
            .unwrap()
        })
        .collect::<Vec<_>>();
    let workspaces = Arc::new(Workspaces {
        records: coordinates
            .iter()
            .enumerate()
            .filter(|(i, _)| i % 2 == 0)
            .map(|(_, coordinates)| {
                let record = WorkspaceRecord::new(coordinates.clone());
                (record.id.clone(), record)
            })
            .collect(),
        ..Default::default()
    });
    Arc::get_mut(&mut owner).unwrap().workspace = workspaces.clone();
    let selected = coordinates
        .iter()
        .map(Some)
        .chain([None, Some(&coordinates[0])]);
    let lookups = owner.workspace_lookups(selected.clone());
    assert_eq!(workspaces.calls.load(SeqCst), 0);
    let rows = lookups.try_collect::<Vec<_>>().await.unwrap();
    assert_eq!(
        rows,
        selected
            .map(|coordinates| coordinates.and_then(|coordinates| {
                let id = WorkspaceId::from_coordinates(coordinates);
                workspaces.records.contains_key(&id).then_some(id)
            }))
            .collect::<Vec<_>>()
    );
    assert_eq!(workspaces.calls.swap(0, SeqCst), 12);
    assert_eq!(workspaces.reads.peak.load(SeqCst), 4);
    let mut lookups = owner.workspace_lookups(coordinates.iter().map(Some));
    lookups.next().await.unwrap().unwrap();
    drop(lookups);
    assert!(workspaces.calls.load(SeqCst) <= 4);
}
#[tokio::test]
async fn empty_search_page_advances_last_scanned_position_and_fences_query_and_epochs() {
    let owner = owner().await;
    let filter = NavigationFilter {
        query: "row-001".into(),
        ..Default::default()
    };
    let first = owner
        .query(&CallOrigin::Local, filter.clone(), None)
        .unwrap()
        .await
        .unwrap();
    assert!(first.entries.is_empty());
    assert_eq!(first.scanned, 256);
    let cursor = first.next.unwrap();
    assert_eq!(cursor.token.len(), 32);
    assert!(cursor.after.is_none());
    assert_eq!(
        owner
            .cursors
            .lock()
            .unwrap()
            .read(&CallOrigin::Local, &cursor)
            .unwrap()
            .session_id
            .as_str(),
        "row-045"
    );
    let last = owner
        .query(&CallOrigin::Local, filter.clone(), Some(cursor.clone()))
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
            owner
                .query(&CallOrigin::Local, filter.clone(), Some(wrong))
                .unwrap()
                .await,
            Err(ApiError::Invalid(_))
        ));
    }
    owner.close().await;
    assert!(matches!(
        owner.query(&CallOrigin::Local, filter, None),
        Err(ApiError::ShuttingDown)
    ));
}
#[tokio::test]
async fn match_limit_continues_after_64_not_after_entire_scanned_store_page() {
    let owner = owner().await;
    let mut after = None;
    let mut found = Vec::new();
    loop {
        let page = owner
            .query(&CallOrigin::Local, NavigationFilter::default(), after)
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

#[tokio::test]
async fn activity_moving_ahead_of_a_live_cursor_is_exposed_on_refresh() {
    let reads = Arc::new(ReadConcurrency::default());
    let owner = owner_with_reads(reads.clone()).await;
    let filter = NavigationFilter::default();
    let first = owner
        .query(&CallOrigin::Local, filter.clone(), None)
        .unwrap()
        .await
        .unwrap();
    assert_eq!(
        first.newest.as_ref().unwrap().session_id.as_str(),
        "row-300"
    );
    let cursor = first.next.unwrap();
    let session = SessionId::new("row-001").unwrap();
    owner
        .store
        .append(AppendBatch {
            session_id: session.clone(),
            expected_seq: 1,
            header: None,
            facts: vec![Arc::new(
                SessionFact::new(
                    2,
                    1000,
                    SessionFactBody::CancelRequested {
                        turn_id: TurnId::new("turn").unwrap(),
                        reason: None,
                    },
                )
                .unwrap(),
            )],
        })
        .await
        .unwrap();
    let continued = owner
        .query(&CallOrigin::Local, filter.clone(), Some(cursor))
        .unwrap()
        .await
        .unwrap();
    assert_eq!(continued.newest.as_ref().unwrap().session_id, session);
    assert!(
        !continued
            .entries
            .iter()
            .any(|entry| entry.session == session)
    );
    let refreshed = owner
        .query(&CallOrigin::Local, filter, None)
        .unwrap()
        .await
        .unwrap();
    assert_eq!(refreshed.entries[0].session, session);
    assert_eq!(refreshed.entries[0].created_at_ms, "1");
    assert_eq!(refreshed.entries[0].last_activity_ms, "1000");
    assert_eq!(reads.peak.load(std::sync::atomic::Ordering::SeqCst), 0);
    let absent = owner
        .query(
            &CallOrigin::Local,
            NavigationFilter {
                workspace: WorkspaceFilter::Registered {
                    id: WorkspaceId::from_canonical_path(std::path::Path::new("/gone")).unwrap(),
                },
                ..Default::default()
            },
            None,
        )
        .unwrap()
        .await
        .unwrap();
    assert_eq!(absent.scanned, 0);
    assert!(absent.entries.is_empty() && absent.next.is_none());
    owner.close().await;
}

#[tokio::test]
async fn manual_seed_is_complete_before_ordered_64_65_summary_reads() {
    use rsi_navigation_api::{OrderMembership, OrderScope, SummaryRequest};
    let owner = owner().await;
    owner.state.lock().unwrap().document = Arc::new(Document {
        accounting: None,
        revision: 1,
        records: [(
            SessionId::new("row-065").unwrap(),
            SessionMetadata {
                pinned: true,
                ..Default::default()
            },
        )]
        .into(),
    });
    let seed = owner
        .order_seed(&CallOrigin::Local, OrderScope::All)
        .unwrap()
        .await
        .unwrap();
    seed.validate().unwrap();
    let OrderMembership::Available { members, .. } = seed.membership else {
        panic!("bounded seed")
    };
    assert_eq!(members.len(), 300);
    assert_eq!(members[64].session.as_str(), "row-065");
    assert!(members[64].pinned);
    let ids: Vec<_> = members
        .iter()
        .rev()
        .map(|member| member.session.clone())
        .collect();
    for page in ids.chunks(64) {
        let read = owner
            .summaries(
                &CallOrigin::Local,
                SummaryRequest {
                    sessions: page.to_vec(),
                    metadata_revision: "1".into(),
                },
            )
            .unwrap()
            .await
            .unwrap();
        assert_eq!(
            read.entries
                .into_iter()
                .map(|row| row.unwrap().session)
                .collect::<Vec<_>>(),
            page
        );
    }
    assert!(
        owner
            .summaries(
                &CallOrigin::Local,
                SummaryRequest {
                    sessions: ids[..65].to_vec(),
                    metadata_revision: "1".into()
                }
            )
            .is_err()
    );
    assert!(
        owner
            .summaries(
                &CallOrigin::Local,
                SummaryRequest {
                    sessions: ids[..64].to_vec(),
                    metadata_revision: "0".into()
                }
            )
            .unwrap()
            .await
            .is_err()
    );
    let absent = owner
        .summaries(
            &CallOrigin::Local,
            SummaryRequest {
                sessions: vec![SessionId::new("absent").unwrap()],
                metadata_revision: "1".into(),
            },
        )
        .unwrap()
        .await
        .unwrap();
    assert_eq!(absent.entries, [None]);
    owner.close().await;
}
#[test]
fn metadata_bounds_reject_durable_and_external_oversize_or_control_titles() {
    for title in [String::new(), "中".repeat(86), "line\nbreak".into()] {
        assert!(
            SessionMetadata {
                pinned: false,
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
            accounting: None,
            revision: 1,
            records
        }
        .validate()
        .is_err()
    );
}

#[tokio::test]
async fn pins_without_authorizable_headers_are_hidden_and_metadata_is_retained() {
    let owner = owner().await;
    let metadata = SessionMetadata {
        title: Some("Pinned project".into()),
        archived: false,
        pinned: true,
    };
    let records = ["row-001", "row-300", "gone"]
        .map(|id| (SessionId::new(id).unwrap(), metadata.clone()))
        .into();
    owner.state.lock().unwrap().document = Arc::new(Document {
        accounting: None,
        revision: 7,
        records,
    });
    let pins = owner
        .pinned(CallOrigin::Local, NavigationFilter::default())
        .unwrap()
        .await
        .unwrap();
    assert_eq!(pins.metadata_revision, "7");
    assert_eq!(pins.entries.len(), 2);
    assert!(
        matches!(&pins.entries[0], PinnedEntry::Available {entry} if entry.session.as_str()=="row-300")
    );
    assert!(
        matches!(&pins.entries[1], PinnedEntry::Available {entry} if entry.session.as_str()=="row-001")
    );
    assert!(
        owner
            .state
            .lock()
            .unwrap()
            .document
            .records
            .contains_key(&SessionId::new("gone").unwrap())
    );
    let mut filter = NavigationFilter {
        workspace: WorkspaceFilter::Unregistered,
        ..Default::default()
    };
    let unregistered = owner
        .pinned(CallOrigin::Local, filter.clone())
        .unwrap()
        .await
        .unwrap();
    assert_eq!(unregistered.entries.len(), 2);
    filter.query = "row-001".into();
    assert_eq!(
        owner
            .pinned(CallOrigin::Local, filter.clone())
            .unwrap()
            .await
            .unwrap()
            .entries
            .len(),
        1
    );
    assert!(
        owner
            .query(&CallOrigin::Local, filter, None)
            .unwrap()
            .await
            .unwrap()
            .entries
            .is_empty()
    );
    let page = owner
        .query(&CallOrigin::Local, NavigationFilter::default(), None)
        .unwrap()
        .await
        .unwrap();
    assert!(!page.entries.iter().any(|entry| entry.metadata.pinned));
    assert_eq!(
        owner.state.lock().unwrap().document.records.len(),
        3,
        "queries never remove missing pins"
    );
    owner.close().await;
}
#[test]
fn legacy_metadata_and_pin_capacity_have_explicit_durable_semantics() {
    let legacy: Document = serde_json::from_value(
        serde_json::json!({"revision":1,"records":{"old":{"title":null,"archived":false}}}),
    )
    .unwrap();
    assert!(!legacy.records.values().next().unwrap().pinned);
    assert_eq!(
        serde_json::to_value(&legacy).unwrap()["records"]["old"]["pinned"],
        false
    );
    let mut document = Document::default();
    for id in 0..64 {
        document.records.insert(
            SessionId::new(format!("pin-{id}")).unwrap(),
            SessionMetadata {
                pinned: true,
                ..Default::default()
            },
        );
    }
    assert!(document.validate().is_ok());
    document.records.insert(
        SessionId::new("pin-65").unwrap(),
        SessionMetadata {
            pinned: true,
            ..Default::default()
        },
    );
    assert!(document.validate().is_err());
    assert!(
        SessionMetadata {
            pinned: true,
            archived: true,
            ..Default::default()
        }
        .validate()
        .is_err()
    );
}

#[tokio::test]
async fn pinned_summaries_preserve_sorted_order_without_attaching_sessions() {
    let reads = Arc::new(ReadConcurrency::default());
    let owner = owner_with_reads(reads.clone()).await;
    owner.state.lock().unwrap().document = Arc::new(Document {
        accounting: None,
        revision: 1,
        records: (1..=64)
            .map(|id| {
                (
                    SessionId::new(format!("row-{id:03}")).unwrap(),
                    SessionMetadata {
                        pinned: true,
                        ..Default::default()
                    },
                )
            })
            .collect(),
    });
    let pins = owner
        .pinned(CallOrigin::Local, NavigationFilter::default())
        .unwrap()
        .await
        .unwrap();
    assert_eq!(pins.entries.len(), 64);
    assert_eq!(reads.peak.load(std::sync::atomic::Ordering::SeqCst), 0);
    assert_eq!(reads.active.load(std::sync::atomic::Ordering::SeqCst), 0);
    for (offset, entry) in pins.entries.iter().enumerate() {
        assert!(
            matches!(entry, PinnedEntry::Available {entry} if entry.session.as_str() == format!("row-{:03}", 64-offset))
        );
    }
    owner.close().await;
}

#[derive(Debug)]
struct RecordingDomain(DomainSpec, Mutex<Option<serde_json::Value>>);
#[async_trait]
impl Domain for RecordingDomain {
    fn ensure_available(&self) -> std::result::Result<(), rsi_storage::StorageError> {
        Ok(())
    }
    fn spec(&self) -> &DomainSpec {
        &self.0
    }
    async fn snapshot(
        &self,
    ) -> std::result::Result<BTreeMap<String, serde_json::Value>, rsi_storage::StorageError> {
        panic!("unexpected snapshot")
    }
    async fn put(
        &self,
        key: &str,
        value: serde_json::Value,
    ) -> std::result::Result<(), rsi_storage::StorageError> {
        assert_eq!(key, "metadata");
        *self.1.lock().unwrap() = Some(value);
        Ok(())
    }
    async fn delete(&self, _: &str) -> std::result::Result<bool, rsi_storage::StorageError> {
        panic!("unexpected deletion")
    }
}
#[tokio::test]
async fn missing_unpinned_metadata_can_be_cleared_with_revision_cas() {
    let mut owner = owner().await;
    let domain = Arc::new(RecordingDomain(
        owner.domain.spec().clone(),
        Mutex::new(None),
    ));
    Arc::get_mut(&mut owner).unwrap().domain = domain.clone();
    let missing = SessionId::new("gone").unwrap();
    owner.state.lock().unwrap().document = Arc::new(Document {
        accounting: None,
        revision: 7,
        records: [(
            missing.clone(),
            SessionMetadata {
                title: Some("Old archived conversation".into()),
                archived: true,
                pinned: false,
            },
        )]
        .into(),
    });
    assert!(
        owner
            .replace(
                CallOrigin::Local,
                missing.clone(),
                "6",
                SessionMetadata::default()
            )
            .unwrap()
            .await
            .is_err()
    );
    assert!(domain.1.lock().unwrap().is_none());
    owner
        .replace(
            CallOrigin::Local,
            missing.clone(),
            "7",
            SessionMetadata::default(),
        )
        .unwrap()
        .await
        .unwrap();
    assert!(owner.state.lock().unwrap().document.records.is_empty());
    let saved: Document =
        serde_json::from_value(domain.1.lock().unwrap().clone().unwrap()).unwrap();
    assert!(saved.records.is_empty());
    assert_eq!(saved.revision, 8);
    assert!(
        owner
            .replace(
                CallOrigin::Local,
                missing,
                "8",
                SessionMetadata {
                    title: Some("fabricated".into()),
                    ..Default::default()
                }
            )
            .unwrap()
            .await
            .is_err()
    );
    owner.close().await;
}

#[derive(Debug, Default)]
struct Visibility;
impl rsi_execution::ExecutionResolver for Visibility {
    fn visibility(&self, origin: &CallOrigin) -> Result<rsi_execution::ExecutionVisibility> {
        let permit = self.admit(origin, &rsi_execution::ExecutionLocation::Local)?;
        let locations = match origin {
            CallOrigin::Local => rsi_execution::ExecutionLocations::all(),
            CallOrigin::Device(_) => {
                rsi_execution::ExecutionLocations::only(std::collections::BTreeSet::from([
                    rsi_execution::ExecutionLocation::Local,
                ]))
                .unwrap()
            }
        };
        Ok(rsi_execution::ExecutionVisibility::new(locations, permit))
    }
    fn admit(
        &self,
        origin: &CallOrigin,
        location: &rsi_execution::ExecutionLocation,
    ) -> Result<rsi_execution::ExecutionOperation> {
        if matches!(origin, CallOrigin::Device(device) if device.revoked.is_cancelled() || *location != rsi_execution::ExecutionLocation::Local)
        {
            return Err(ApiError::Unauthorized);
        }
        Ok(rsi_execution::ExecutionOperation::new(()))
    }
    fn lease(
        &self,
        _: CallOrigin,
        _: &rsi_execution::ExecutionLocation,
    ) -> Result<rsi_execution::ExecutionLease> {
        panic!("Navigation opened an execution connection")
    }
}

#[tokio::test]
async fn visibility_filters_before_order_capacity_cursors_pins_and_exact_summaries() {
    use rsi_execution::ExecutionLocation;
    use rsi_navigation_api::{OrderMembership, OrderScope, SummaryRequest};
    let owner = owner().await;
    let hidden = hidden_sessions(&owner).await;
    let origin = CallOrigin::Device(rsi_api_protocol::AuthenticatedDevice {
        id: rsi_api_protocol::DeviceId::from_bytes([1; 16]),
        revoked: tokio_util::sync::CancellationToken::new(),
    });
    let page = owner
        .query(&origin, NavigationFilter::default(), None)
        .unwrap()
        .await
        .unwrap();
    assert_eq!(page.entries.len(), 64);
    assert_eq!(page.newest.unwrap().session_id.as_str(), "row-300");
    assert_eq!(page.next.unwrap().token.len(), 32);
    let seed = owner
        .order_seed(&origin, OrderScope::All)
        .unwrap()
        .await
        .unwrap();
    let OrderMembership::Available { members, groups } = seed.membership else {
        panic!("hidden rows consumed the seed budget");
    };
    assert_eq!(members.len(), 300);
    assert!(
        groups
            .iter()
            .all(|coordinates| *coordinates.location() == ExecutionLocation::Local)
    );
    let summaries = owner
        .summaries(
            &origin,
            SummaryRequest {
                metadata_revision: "0".into(),
                sessions: vec![hidden[0].clone(), SessionId::new("row-001").unwrap()],
            },
        )
        .unwrap()
        .await
        .unwrap();
    assert!(summaries.entries[0].is_none());
    assert!(summaries.entries[1].is_some());
    {
        let mut state = owner.state.lock().unwrap();
        let mut document = (*state.document).clone();
        document.records.insert(
            hidden[0].clone(),
            SessionMetadata {
                pinned: true,
                ..Default::default()
            },
        );
        state.document = Arc::new(document);
    }
    assert!(
        owner
            .pinned(origin, NavigationFilter::default())
            .unwrap()
            .await
            .unwrap()
            .entries
            .is_empty()
    );
    owner.close().await;
}

async fn hidden_sessions(owner: &Navigation) -> Vec<SessionId> {
    use rsi_execution::{ExecutionCoordinates, ExecutionLocation, ExecutionTargetId};
    let mut hidden = Vec::new();
    for index in 0..1025 {
        let header = SessionHeader::new(
            SessionId::new(format!("hidden-{index:04}")).unwrap(),
            400 + index,
            ExecutionCoordinates::new(
                ExecutionLocation::Ssh {
                    target: ExecutionTargetId::parse("a".repeat(32)).unwrap(),
                },
                "/secret-remote",
            )
            .unwrap(),
            AgentPresetId::new("fixture").unwrap(),
            FrozenAgentSettings::new(
                "fixture",
                "system",
                rsi_ai_protocol::ModelRef::new("fixture", "model").unwrap(),
                rsi_sandbox::SandboxMode::ReadOnly,
                false,
            )
            .unwrap(),
        )
        .unwrap();
        owner
            .store
            .append(AppendBatch {
                session_id: header.session_id().clone(),
                expected_seq: 0,
                header: Some(header.clone()),
                facts: vec![Arc::new(
                    SessionFact::new(
                        1,
                        header.created_at_ms(),
                        SessionFactBody::TurnAccepted {
                            turn_id: TurnId::new("turn").unwrap(),
                            text: "hidden".into(),
                            model: None,
                            reasoning_effort: None,
                            sandbox: rsi_sandbox::SandboxMode::ReadOnly,
                            require_approval: false,
                        },
                    )
                    .unwrap(),
                )],
            })
            .await
            .unwrap();
        hidden.push(header.session_id().clone());
    }
    hidden
}

#[test]
fn metadata_incremental_sizes_match_the_durable_wrapper_through_edits() {
    let mut document = Document {
        revision: 8,
        ..Document::default()
    };
    for (id, title, pinned) in [
        ("one", Some("中文\n\"escaped"), true),
        ("two", Some("other"), true),
        ("one", Some("short"), false),
        ("two", None, false),
        ("absent", None, false),
    ] {
        let metadata = SessionMetadata {
            title: title.map(str::to_owned),
            pinned,
            archived: false,
        };
        document
            .edit(&SessionId::new(id).unwrap(), &metadata)
            .unwrap();
        let accounting = document.accounting.unwrap();
        assert_eq!(
            accounting.records.bytes(),
            serde_json::to_vec(&document.records).unwrap().len()
        );
        assert_eq!(accounting.records.records(), document.records.len());
        assert_eq!(
            accounting.pins,
            document.records.values().filter(|row| row.pinned).count()
        );
        let expected = serde_json::to_vec(&serde_json::json!({"metadata": &document}))
            .unwrap()
            .len();
        assert_eq!(
            expected,
            b"{\"metadata\":{\"revision\":,\"records\":}}".len()
                + document.revision.to_string().len()
                + accounting.records.bytes()
        );
    }
    let pinned = SessionMetadata {
        pinned: true,
        ..Default::default()
    };
    for id in 0..64 {
        document
            .edit(&SessionId::new(format!("pin-{id}")).unwrap(), &pinned)
            .unwrap();
    }
    let before = serde_json::to_value(&document).unwrap();
    assert!(
        document
            .edit(&SessionId::new("overflow").unwrap(), &pinned)
            .is_err()
    );
    assert_eq!(before, serde_json::to_value(&document).unwrap());
    document.revision = u64::MAX;
    assert!(
        document
            .edit(
                &SessionId::new("pin-0").unwrap(),
                &SessionMetadata::default()
            )
            .is_err()
    );
    assert_eq!(document.accounting.unwrap().pins, 64);
}

#[derive(Debug)]
struct ProtectedView;
impl rsi_session_protocol::SessionProtection for ProtectedView {
    fn view(
        &self,
        _: &rsi_agent_session_protocol::SessionProtectionScope,
        origin: &CallOrigin,
    ) -> rsi_session_protocol::Result<tokio_util::sync::CancellationToken> {
        if matches!(origin, CallOrigin::Local) {
            Ok(tokio_util::sync::CancellationToken::new())
        } else {
            Err(SessionError::Api(ApiError::Unauthorized))
        }
    }
}
#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "Exercise all protected navigation surfaces in one authorization scenario"
)]
async fn protected_scope_is_hidden_in_pages_pins_order_membership_and_exact_summaries() {
    #[derive(Debug)]
    struct Unavailable;
    impl rsi_session_protocol::SessionProtection for Unavailable {
        fn view(
            &self,
            _: &rsi_agent_session_protocol::SessionProtectionScope,
            _: &CallOrigin,
        ) -> rsi_session_protocol::Result<tokio_util::sync::CancellationToken> {
            Err(SessionError::Api(ApiError::Unavailable))
        }
    }
    let mut owner = owner().await;
    Arc::get_mut(&mut owner).unwrap().protection = Some(Arc::new(ProtectedView));
    let header = SessionHeader::new_local(
        SessionId::new("protected").unwrap(),
        1000,
        "/protected-only-workspace",
        AgentPresetId::new("fixture").unwrap(),
        FrozenAgentSettings::new(
            "fixture",
            "system",
            rsi_ai_protocol::ModelRef::new("fixture", "model").unwrap(),
            rsi_sandbox::SandboxMode::ReadOnly,
            false,
        )
        .unwrap(),
    )
    .unwrap()
    .with_protection(
        rsi_agent_session_protocol::SessionProtectionScope::new("automation", "source:rule")
            .unwrap(),
    )
    .unwrap();
    owner
        .store
        .append(AppendBatch {
            session_id: header.session_id().clone(),
            expected_seq: 0,
            header: Some(header.clone()),
            facts: vec![Arc::new(
                SessionFact::new(
                    1,
                    1000,
                    SessionFactBody::TurnAccepted {
                        turn_id: TurnId::new("turn").unwrap(),
                        text: "hidden".into(),
                        model: None,
                        reasoning_effort: None,
                        sandbox: rsi_sandbox::SandboxMode::ReadOnly,
                        require_approval: false,
                    },
                )
                .unwrap(),
            )],
        })
        .await
        .unwrap();
    let device = CallOrigin::Device(rsi_api_protocol::AuthenticatedDevice {
        id: rsi_api_protocol::DeviceId::from_bytes([9; 16]),
        revoked: tokio_util::sync::CancellationToken::new(),
    });
    let local = owner
        .query(&CallOrigin::Local, NavigationFilter::default(), None)
        .unwrap()
        .await
        .unwrap();
    assert_eq!(local.entries[0].session, header.session_id().clone());
    let hidden = owner
        .query(&device, NavigationFilter::default(), None)
        .unwrap()
        .await
        .unwrap();
    assert!(
        !hidden
            .entries
            .iter()
            .any(|row| row.session == *header.session_id())
    );
    assert!(hidden.newest.is_none());
    let summaries = owner
        .summaries(
            &device,
            rsi_navigation_api::SummaryRequest {
                sessions: vec![header.session_id().clone()],
                metadata_revision: "0".into(),
            },
        )
        .unwrap()
        .await
        .unwrap();
    assert_eq!(summaries.entries, vec![None]);
    let seed = owner
        .order_seed(&device, rsi_navigation_api::OrderScope::All)
        .unwrap()
        .await
        .unwrap();
    let encoded = serde_json::to_string(&seed).unwrap();
    assert!(!encoded.contains("protected"));
    owner.state.lock().unwrap().document = Arc::new(Document {
        records: BTreeMap::from([(
            header.session_id().clone(),
            SessionMetadata {
                pinned: true,
                ..Default::default()
            },
        )]),
        ..Default::default()
    });
    assert!(
        owner
            .pinned(device, NavigationFilter::default())
            .unwrap()
            .await
            .unwrap()
            .entries
            .is_empty()
    );
    Arc::get_mut(&mut owner).unwrap().protection = Some(Arc::new(Unavailable));
    assert!(matches!(
        owner
            .query(&CallOrigin::Local, NavigationFilter::default(), None)
            .unwrap()
            .await,
        Err(ApiError::Unavailable)
    ));
    owner.close().await;
}

#[tokio::test]
async fn scan_cursors_hide_store_cuts_and_reject_forgery_foreign_callers_and_eviction() {
    let owner = owner().await;
    let page = owner
        .query(&CallOrigin::Local, NavigationFilter::default(), None)
        .unwrap()
        .await
        .unwrap();
    let cursor = page.next.unwrap();
    assert_eq!(cursor.token.len(), 32);
    let foreign = CallOrigin::Device(rsi_api_protocol::AuthenticatedDevice {
        id: rsi_api_protocol::DeviceId::from_bytes([8; 16]),
        revoked: tokio_util::sync::CancellationToken::new(),
    });
    assert!(
        owner
            .query(&foreign, cursor.filter.clone(), Some(cursor.clone()))
            .unwrap()
            .await
            .is_err()
    );
    let mut forged = cursor.clone();
    forged.token = "f".repeat(32);
    assert!(
        owner
            .query(&CallOrigin::Local, forged.filter.clone(), Some(forged))
            .unwrap()
            .await
            .is_err()
    );
    for _ in 0..128 {
        owner
            .query(&CallOrigin::Local, NavigationFilter::default(), None)
            .unwrap()
            .await
            .unwrap();
    }
    assert!(
        owner
            .query(&CallOrigin::Local, cursor.filter.clone(), Some(cursor))
            .unwrap()
            .await
            .is_err()
    );
    owner.close().await;
}

#[tokio::test]
async fn one_principal_cannot_evict_another_principals_scan() {
    let owner = owner().await;
    let cursor = owner
        .query(&CallOrigin::Local, NavigationFilter::default(), None)
        .unwrap()
        .await
        .unwrap()
        .next
        .unwrap();
    let foreign = CallOrigin::Device(rsi_api_protocol::AuthenticatedDevice {
        id: rsi_api_protocol::DeviceId::from_bytes([8; 16]),
        revoked: tokio_util::sync::CancellationToken::new(),
    });
    for _ in 0..130 {
        owner
            .query(&foreign, NavigationFilter::default(), None)
            .unwrap()
            .await
            .unwrap();
    }
    owner
        .query(&CallOrigin::Local, cursor.filter.clone(), Some(cursor))
        .unwrap()
        .await
        .expect("foreign scans must preserve this caller's continuation");
    owner.close().await;
}

#[tokio::test(start_paused = true)]
async fn a_linear_scan_replaces_its_cut_and_expires_after_five_minutes() {
    let mut book = CursorBook::default();
    let owner = owner().await;
    let cursor = owner
        .query(&CallOrigin::Local, NavigationFilter::default(), None)
        .unwrap()
        .await
        .unwrap()
        .next
        .unwrap();
    let cut = owner
        .cursors
        .lock()
        .unwrap()
        .read(&CallOrigin::Local, &cursor)
        .unwrap();
    let mut current = book
        .issue(
            &CallOrigin::Local,
            cursor.filter.clone(),
            cursor.host_epoch.clone(),
            cursor.metadata_revision.clone(),
            cut.clone(),
            None,
            None,
        )
        .unwrap();
    for _ in 0..130 {
        let previous = current.clone();
        current = book
            .issue(
                &CallOrigin::Local,
                cursor.filter.clone(),
                cursor.host_epoch.clone(),
                cursor.metadata_revision.clone(),
                cut.clone(),
                None,
                Some(&previous),
            )
            .unwrap();
        assert!(book.read(&CallOrigin::Local, &previous).is_err());
        assert_eq!(book.cuts.len(), 1);
    }
    tokio::time::advance(std::time::Duration::from_secs(299)).await;
    assert!(book.read(&CallOrigin::Local, &current).is_ok());
    tokio::time::advance(std::time::Duration::from_secs(1)).await;
    assert!(book.read(&CallOrigin::Local, &current).is_err());
    owner.close().await;
}

#[test]
fn header_failure_is_not_silently_treated_as_a_protection_denial() {
    use rsi_agent_store_protocol::StoreError;
    assert!(matches!(
        visible_header(Err(StoreError::NotFound("missing".into()))),
        Ok(None)
    ));
    assert!(matches!(
        visible_header(Err(StoreError::ValidationBusy)),
        Err(ApiError::Unavailable)
    ));
}

#[tokio::test(start_paused = true)]
async fn cursor_successors_and_refreshes_preserve_both_caps_at_global_capacity() {
    let mut book = CursorBook::default();
    let owner = CallOrigin::Local;
    let issue =
        |book: &mut CursorBook, origin: &CallOrigin, previous: Option<&NavigationCursor>| {
            book.issue(
                origin,
                NavigationFilter::default(),
                HostEpoch::from_bytes([1; 16]),
                "0".into(),
                rsi_agent_store_protocol::StoreActivityCursor {
                    last_activity_ms: 1,
                    session_id: SessionId::new("hidden").unwrap(),
                },
                None,
                previous,
            )
        };
    for _ in 0..8 {
        issue(&mut book, &owner, None).unwrap();
    }
    for id in 1..=120 {
        issue(
            &mut book,
            &CallOrigin::Device(rsi_api_protocol::AuthenticatedDevice {
                id: rsi_api_protocol::DeviceId::from_bytes([id; 16]),
                revoked: tokio_util::sync::CancellationToken::new(),
            }),
            None,
        )
        .unwrap();
    }
    assert_eq!(book.cuts.len(), 128);
    let first = book.cuts.front().unwrap().issued.clone();
    let foreign = book.cuts.back().unwrap().issued.clone();
    let successor = issue(&mut book, &owner, Some(&first)).unwrap();
    assert_eq!(book.cuts.len(), 128);
    assert_eq!(
        book.cuts.iter().filter(|c| c.principal == "local").count(),
        8
    );
    assert!(book.read(&owner, &first).is_err());
    issue(&mut book, &owner, None).unwrap();
    assert_eq!(book.cuts.len(), 128);
    assert!(book.read(&owner, &successor).is_ok());
    assert!(book.cuts.iter().any(|c| c.issued == foreign));
}
