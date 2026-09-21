use super::*;
use rsi_acp_protocol::{
    observation::{Capabilities, ConversationId, Snapshot},
    service::{self, Endpoint, Resident, Setup, View},
};
use rsi_session_protocol::{CreateSession, RecentSessionCursor, RecentSessionPage, SessionHandle};
#[derive(Debug)]
struct Sources(Mutex<ExternalStatus>);
#[async_trait]
impl SessionService for Sources {
    async fn create(
        &self,
        _: CreateSession,
    ) -> rsi_session_protocol::Result<Arc<dyn SessionHandle>> {
        unreachable!()
    }
    async fn attach(
        &self,
        _: &rsi_agent_session_protocol::SessionId,
    ) -> rsi_session_protocol::Result<Arc<dyn SessionHandle>> {
        unreachable!()
    }
    async fn list_recent(
        &self,
        _: Option<&RecentSessionCursor>,
        _: usize,
    ) -> rsi_session_protocol::Result<RecentSessionPage> {
        unreachable!()
    }
    async fn activity(
        &self,
    ) -> rsi_session_protocol::Result<rsi_session_protocol::SessionActivityPage> {
        Ok(rsi_session_protocol::SessionActivityPage {
            entries: vec![],
            truncated: false,
        })
    }
}
#[async_trait]
impl ExternalConversations for Sources {
    async fn residents(&self) -> service::Result<Vec<Resident>> {
        let status = *self.0.lock().unwrap();
        Ok(vec![Resident {
            snapshot: Snapshot {
                id: ConversationId::new("external").unwrap(),
                endpoint: "test".into(),
                cwd: "/workspace".into(),
                remote: None,
                generation: 1,
                epoch: 2,
                status,
                completion: None,
                capabilities: Capabilities::default(),
            },
            sequence: "2".into(),
            connected: status != ExternalStatus::Closed,
            permissions: vec![],
        }])
    }
    async fn endpoints(&self) -> service::Result<Vec<Endpoint>> {
        unreachable!()
    }
    async fn list(&self, _: Option<ConversationId>) -> service::Result<Vec<Snapshot>> {
        unreachable!()
    }
    async fn view(&self, _: &ConversationId) -> service::Result<View> {
        unreachable!()
    }
    async fn start(&self, _: ConversationId, _: &str) -> service::Result<Snapshot> {
        unreachable!()
    }
    async fn reconnect(&self, _: &ConversationId, _: Setup) -> service::Result<Snapshot> {
        unreachable!()
    }
    async fn submit(&self, _: &ConversationId, _: &str) -> service::Result<Snapshot> {
        unreachable!()
    }
    async fn cancel(&self, _: &ConversationId) -> service::Result<Snapshot> {
        unreachable!()
    }
    async fn close(&self, _: &ConversationId) -> service::Result<Snapshot> {
        unreachable!()
    }
    async fn answer(&self, _: &ConversationId, _: u64, _: &str, _: &str) -> service::Result<()> {
        unreachable!()
    }
    async fn page(
        &self,
        _: &ConversationId,
        _: u64,
        _: u64,
    ) -> service::Result<rsi_acp_protocol::observation::Page> {
        unreachable!()
    }
    async fn window(
        &self,
        _: &ConversationId,
        _: u64,
        _: u64,
        _: usize,
    ) -> service::Result<Vec<u8>> {
        unreachable!()
    }
}
#[derive(Debug)]
struct Storage {
    spec: DomainSpec,
    rows: Mutex<BTreeMap<String, serde_json::Value>>,
    fail: std::sync::atomic::AtomicBool,
}
#[async_trait]
impl Domain for Storage {
    fn spec(&self) -> &DomainSpec {
        &self.spec
    }
    async fn snapshot(&self) -> BTreeMap<String, serde_json::Value> {
        self.rows.lock().unwrap().clone()
    }
    async fn put(
        &self,
        key: &str,
        value: serde_json::Value,
    ) -> std::result::Result<(), rsi_storage::StorageError> {
        self.rows.lock().unwrap().insert(key.into(), value);
        Ok(())
    }
    async fn delete(&self, key: &str) -> std::result::Result<bool, rsi_storage::StorageError> {
        let removed = self.rows.lock().unwrap().remove(key).is_some();
        if self.fail.load(std::sync::atomic::Ordering::Acquire) {
            Err(rsi_storage::StorageError::Io(
                "lost deletion receipt".into(),
            ))
        } else {
            Ok(removed)
        }
    }
}
#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "one causal capacity, read status and lost deletion receipt scenario"
)]
async fn ready_and_closed_history_stays_unread_and_full_positions_admit_new_acknowledgments() {
    let sources = Arc::new(Sources(Mutex::new(ExternalStatus::Ready)));
    let positions: BTreeMap<_, _> = (0..4096)
        .map(|index| {
            (
                format!("{index:064x}"),
                Position {
                    conversation: ConversationIdentity::Native(
                        rsi_agent_session_protocol::SessionId::new(format!("old-{index}")).unwrap(),
                    ),
                    epoch: "0".into(),
                    sequence: "1".into(),
                },
            )
        })
        .collect();
    let storage = Arc::new(Storage {
        spec: DomainSpec {
            id: "test".into(),
            backend: "test".into(),
            version: 1,
            maximum_records: 4096,
            maximum_bytes: 1024 * 1024,
        },
        rows: Mutex::new(
            positions
                .iter()
                .map(|(key, value)| (key.clone(), serde_json::to_value(value).unwrap()))
                .collect(),
        ),
        fail: std::sync::atomic::AtomicBool::new(false),
    });
    let owner = Attention {
        session: sources.clone(),
        external: sources.clone(),
        epoch: HostEpoch::generate().unwrap(),
        domain: storage.clone(),
        state: Mutex::new(State {
            closed: false,
            uncertain: false,
            recency: positions.keys().cloned().collect(),
            positions,
        }),
        execution: Execution::native(tokio::runtime::Handle::current()),
        tasks: TaskTracker::new(),
        slots: Arc::new(Semaphore::new(2)),
        writer: Arc::new(Semaphore::new(1)),
    };
    for status in [ExternalStatus::Ready, ExternalStatus::Closed] {
        *sources.0.lock().unwrap() = status;
        let page = owner.read(&CallOrigin::Local).await.unwrap();
        assert_eq!(page.entries.len(), 1);
        assert_eq!(page.entries[0].status, Status::Unread);
    }
    let position = owner
        .read(&CallOrigin::Local)
        .await
        .unwrap()
        .entries
        .remove(0)
        .position;
    owner
        .mark(
            CallOrigin::Local,
            MarkRead {
                host_epoch: owner.epoch.clone(),
                position: position.clone(),
            },
        )
        .await
        .unwrap();
    assert!(
        owner
            .read(&CallOrigin::Local)
            .await
            .unwrap()
            .entries
            .is_empty()
    );
    let rows = storage.snapshot().await;
    assert_eq!(rows.len(), 4096);
    assert!(!rows.contains_key(&format!("{:064x}", 0)));
    assert!(serde_json::to_vec(&rows).unwrap().len() <= 1024 * 1024);
    // A durable delete with a lost receipt must never authorize another update.
    let stored_key = key(&CallOrigin::Local, &position.conversation);
    {
        let mut state = owner.state.lock().unwrap();
        state.positions.remove(&stored_key);
        state.recency.retain(|key| key != &stored_key);
        let replacement = format!("{:064x}", 4096);
        state
            .positions
            .insert(replacement.clone(), position.clone());
        state.recency.push_back(replacement);
    }
    storage
        .fail
        .store(true, std::sync::atomic::Ordering::Release);
    assert!(matches!(
        owner
            .mark(
                CallOrigin::Local,
                MarkRead {
                    host_epoch: owner.epoch.clone(),
                    position
                }
            )
            .await,
        Err(ApiError::OutcomeUnknown)
    ));
    assert!(matches!(
        owner.read(&CallOrigin::Local).await,
        Err(ApiError::OutcomeUnknown)
    ));
}
