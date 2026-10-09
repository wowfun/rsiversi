use super::*;

#[tokio::test]
async fn an_accepted_mark_commits_when_retirement_starts_before_dispatch() {
    let (_, storage, owner) = fixture(0);
    let position = owner
        .read(&CallOrigin::Local)
        .await
        .unwrap()
        .entries
        .remove(0)
        .position;
    let accepted = owner
        .mark(
            CallOrigin::Local,
            MarkRead {
                host_epoch: owner.epoch.clone(),
                position: position.clone(),
            },
        )
        .unwrap();
    let mut close = Box::pin(owner.close());
    assert!(futures_util::poll!(&mut close).is_pending());
    assert_eq!(accepted.await.unwrap(), position);
    close.await;
    assert_eq!(storage.rows.lock().unwrap().len(), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn attention_read_tracking_can_be_polled_without_an_entered_tokio_runtime() {
    let (_, _, owner) = fixture(0);
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
        assert_eq!(owner.admission.slots.available_permits(), 1);
        drop(pending);
        executor.block_on(owner.close());
        assert_eq!(owner.admission.slots.available_permits(), 2);
        assert!(owner.admission.tasks.is_empty());
    })
    .join()
    .unwrap();
}

#[tokio::test]
async fn read_waiter_loss_releases_attention_capacity_and_unpolled_reads_do_not_hold_close() {
    let (_, _, owner) = fixture(0);
    let mut read = owner
        .run_read(|_| Box::pin(std::future::pending::<Result<()>>()))
        .unwrap();
    assert!(futures_util::poll!(&mut read).is_pending());
    assert_eq!(owner.admission.slots.available_permits(), 1);
    drop(read);
    tokio::time::timeout(std::time::Duration::from_secs(1), async {
        while !owner.admission.tasks.is_empty() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(owner.admission.slots.available_permits(), 2);
    assert!(owner.admission.tasks.is_empty());
    let unpolled = owner.run_read(|_| Box::pin(async { Ok(()) })).unwrap();
    owner.close().await;
    assert!(matches!(unpolled.await, Err(ApiError::ShuttingDown)));
}
use rsi_acp_protocol::{
    observation::{Capabilities, ConversationId, Snapshot},
    service::{self, Endpoint, Resident, Setup, View},
};
use rsi_session_protocol::SessionService;
use rsi_session_protocol::{CreateSession, RecentSessionCursor, RecentSessionPage, SessionHandle};
#[derive(Clone, Debug)]
struct Sources(
    Arc<Mutex<ExternalStatus>>,
    Arc<std::sync::atomic::AtomicUsize>,
    Arc<Mutex<Vec<CallOrigin>>>,
);
#[async_trait]
impl SessionIngress for Sources {
    fn scoped(&self, origin: CallOrigin) -> Arc<dyn SessionService> {
        self.2.lock().unwrap().push(origin);
        Arc::new(self.clone())
    }
    async fn create_from(
        &self,
        _: CreateSession,
        _: CallOrigin,
    ) -> rsi_session_protocol::Result<Arc<dyn SessionHandle>> {
        unreachable!()
    }
}
#[async_trait]
impl SessionService for Sources {
    async fn read_header(
        &self,
        _: &rsi_agent_session_protocol::SessionId,
    ) -> rsi_session_protocol::Result<rsi_agent_session_protocol::SessionHeader> {
        panic!("unexpected durable Header read")
    }
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
        self.1.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Ok(rsi_session_protocol::SessionActivityPage {
            entries: vec![],
            truncated: false,
        })
    }
}
#[async_trait]
impl ExternalConversations for Sources {
    async fn residents(&self) -> service::Result<Vec<Resident>> {
        self.1.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
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
    fenced: std::sync::atomic::AtomicBool,
    reject_put: std::sync::atomic::AtomicBool,
}
#[async_trait]
impl Domain for Storage {
    fn ensure_available(&self) -> std::result::Result<(), rsi_storage::StorageError> {
        if self.fenced.load(std::sync::atomic::Ordering::Acquire) {
            Err(rsi_storage::StorageError::RecoveryRequired)
        } else {
            Ok(())
        }
    }
    fn spec(&self) -> &DomainSpec {
        &self.spec
    }
    async fn snapshot(
        &self,
    ) -> std::result::Result<BTreeMap<String, serde_json::Value>, rsi_storage::StorageError> {
        self.ensure_available()?;
        Ok(self.rows.lock().unwrap().clone())
    }
    async fn put(
        &self,
        key: &str,
        value: serde_json::Value,
    ) -> std::result::Result<(), rsi_storage::StorageError> {
        self.ensure_available()?;
        if self.reject_put.load(std::sync::atomic::Ordering::Acquire) {
            return Err(rsi_storage::StorageError::Io("rejected before put".into()));
        }
        self.rows.lock().unwrap().insert(key.into(), value);
        Ok(())
    }
    async fn delete(&self, key: &str) -> std::result::Result<bool, rsi_storage::StorageError> {
        self.ensure_available()?;
        let removed = self.rows.lock().unwrap().remove(key).is_some();
        if self.fail.load(std::sync::atomic::Ordering::Acquire) {
            self.fenced
                .store(true, std::sync::atomic::Ordering::Release);
            Err(rsi_storage::StorageError::OutcomeUnknown(
                "lost deletion receipt".into(),
            ))
        } else {
            Ok(removed)
        }
    }
}
fn fixture(count: usize) -> (Arc<Sources>, Arc<Storage>, Arc<Attention>) {
    fixture_with_byte_limit(count, 1024 * 1024)
}
fn fixture_with_byte_limit(
    count: usize,
    maximum_bytes: usize,
) -> (Arc<Sources>, Arc<Storage>, Arc<Attention>) {
    let sources = Arc::new(Sources(
        Arc::new(Mutex::new(ExternalStatus::Ready)),
        Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        Arc::new(Mutex::new(Vec::new())),
    ));
    let positions: BTreeMap<_, _> = (0..count)
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
            maximum_bytes,
        },
        rows: Mutex::new(
            positions
                .iter()
                .map(|(key, value)| (key.clone(), serde_json::to_value(value).unwrap()))
                .collect(),
        ),
        fail: std::sync::atomic::AtomicBool::new(false),
        fenced: std::sync::atomic::AtomicBool::new(false),
        reject_put: std::sync::atomic::AtomicBool::new(false),
    });
    let owner = Arc::new(Attention {
        session: sources.clone(),
        external: sources.clone(),
        epoch: HostEpoch::generate().unwrap(),
        domain: storage.clone(),
        state: Mutex::new(State::from_positions(positions)),
        admission: Admission::new(
            2,
            rsi_meta::Execution::native(tokio::runtime::Handle::current()),
        ),
        writer: Arc::new(Semaphore::new(1)),
    });
    (sources, storage, owner)
}
#[tokio::test]
async fn ready_and_closed_history_stays_unread_and_full_positions_admit_new_acknowledgments() {
    let (sources, storage, owner) = fixture(4096);
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
    finish_mark(owner.mark(
        CallOrigin::Local,
        MarkRead {
            host_epoch: owner.epoch.clone(),
            position: position.clone(),
        },
    ))
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
    let rows = storage.snapshot().await.unwrap();
    assert_eq!(rows.len(), 4096);
    assert!(!rows.contains_key(&format!("{:064x}", 0)));
    assert!(serde_json::to_vec(&rows).unwrap().len() <= 1024 * 1024);
    // A durable delete with a lost receipt must never authorize another update.
    let stored_key = key(&CallOrigin::Local, &position.conversation);
    {
        let mut state = owner.state.lock().unwrap();
        state.remove(&stored_key);
        let replacement = format!("{:064x}", 4096);
        let bytes = position_bytes(&replacement, &position);
        state.put(replacement, position.clone(), bytes);
    }
    storage
        .fail
        .store(true, std::sync::atomic::Ordering::Release);
    assert!(matches!(
        finish_mark(owner.mark(
            CallOrigin::Local,
            MarkRead {
                host_epoch: owner.epoch.clone(),
                position
            }
        ))
        .await,
        Err(ApiError::OutcomeUnknown)
    ));
    assert!(matches!(
        owner.read(&CallOrigin::Local).await,
        Err(ApiError::Unavailable)
    ));
}

#[tokio::test]
async fn fenced_attention_rejects_before_observing_either_source() {
    use std::sync::atomic::Ordering;
    let (sources, storage, owner) = fixture(0);
    let position = owner
        .read(&CallOrigin::Local)
        .await
        .unwrap()
        .entries
        .remove(0)
        .position;
    sources.1.store(0, Ordering::SeqCst);
    storage.fenced.store(true, Ordering::Release);
    assert!(matches!(
        owner.read(&CallOrigin::Local).await,
        Err(ApiError::Unavailable)
    ));
    assert!(matches!(
        finish_mark(owner.mark(
            CallOrigin::Local,
            MarkRead {
                host_epoch: owner.epoch.clone(),
                position,
            }
        ))
        .await,
        Err(ApiError::Unavailable)
    ));
    assert_eq!(sources.1.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn known_failed_acknowledgment_keeps_confirmed_evictions_and_allows_explicit_retry() {
    use std::sync::atomic::Ordering;
    let (_, storage, owner) = fixture(4096);
    let position = owner
        .read(&CallOrigin::Local)
        .await
        .unwrap()
        .entries
        .remove(0)
        .position;
    let request = MarkRead {
        host_epoch: owner.epoch.clone(),
        position: position.clone(),
    };
    storage.reject_put.store(true, Ordering::Release);
    assert!(matches!(
        finish_mark(owner.mark(CallOrigin::Local, request.clone())).await,
        Err(ApiError::Unavailable)
    ));
    let actual = storage.rows.lock().unwrap().clone();
    assert_eq!(actual.len(), 4095);
    assert!(!actual.contains_key(&format!("{:064x}", 0)));
    assert!(!actual.contains_key(&key(&CallOrigin::Local, &position.conversation)));
    {
        let state = owner.state.lock().unwrap();
        let cached: BTreeMap<_, _> = state
            .positions
            .iter()
            .map(|(key, record)| (key.clone(), serde_json::to_value(&record.position).unwrap()))
            .collect();
        assert_eq!(actual, cached);
        assert_eq!(
            state.size.bytes(),
            serde_json::to_vec(&actual).unwrap().len()
        );
        assert_eq!(state.size.records(), actual.len());
        assert_eq!(state.recency.len(), state.positions.len());
        assert!(
            state
                .recency
                .iter()
                .all(|key| state.positions.contains_key(key))
        );
    }
    assert_eq!(
        owner.read(&CallOrigin::Local).await.unwrap().entries.len(),
        1
    );
    storage.reject_put.store(false, Ordering::Release);
    finish_mark(owner.mark(CallOrigin::Local, request))
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
    assert_eq!(storage.rows.lock().unwrap().len(), 4096);
}

#[tokio::test]
async fn byte_limited_eviction_and_repeated_acknowledgments_preserve_exact_cache_sizes() {
    use std::sync::atomic::Ordering;
    let (_, seed, _) = fixture(3);
    let limit = serde_json::to_vec(&seed.snapshot().await.unwrap())
        .unwrap()
        .len();
    let (_, storage, owner) = fixture_with_byte_limit(3, limit);
    let position = owner
        .read(&CallOrigin::Local)
        .await
        .unwrap()
        .entries
        .remove(0)
        .position;
    let request = MarkRead {
        host_epoch: owner.epoch.clone(),
        position,
    };
    let assert_cache = || {
        let actual = storage.rows.lock().unwrap();
        let state = owner.state.lock().unwrap();
        assert_eq!(
            state.size.bytes(),
            serde_json::to_vec(&*actual).unwrap().len()
        );
        assert_eq!(state.size.records(), actual.len());
        assert!(state.size.bytes() <= limit);
        assert_eq!(state.recency.len(), actual.len());
        for (key, record) in &state.positions {
            assert_eq!(actual[key], serde_json::to_value(&record.position).unwrap());
            assert!(state.recency.contains(key));
        }
    };
    assert_cache();
    // The external position is larger than either old native position. One
    // deletion cannot fit it at the initial byte ceiling, so two must commit.
    storage.reject_put.store(true, Ordering::Release);
    assert!(matches!(
        finish_mark(owner.mark(CallOrigin::Local, request.clone())).await,
        Err(ApiError::Unavailable)
    ));
    assert_eq!(storage.rows.lock().unwrap().len(), 1);
    assert!(
        storage
            .rows
            .lock()
            .unwrap()
            .contains_key(&format!("{:064x}", 2))
    );
    assert_cache();
    storage.reject_put.store(false, Ordering::Release);
    for sequence in ["1", "2", "1", "2"] {
        let mut request = request.clone();
        request.position.sequence = sequence.into();
        finish_mark(owner.mark(CallOrigin::Local, request))
            .await
            .unwrap();
        assert_cache();
        assert_eq!(storage.rows.lock().unwrap().len(), 2);
    }
    assert!(
        owner
            .read(&CallOrigin::Local)
            .await
            .unwrap()
            .entries
            .is_empty()
    );
    let state = State::from_positions(
        storage
            .snapshot()
            .await
            .unwrap()
            .into_iter()
            .map(|(key, value)| (key, serde_json::from_value(value).unwrap()))
            .collect(),
    );
    assert_eq!(state.size, owner.state.lock().unwrap().size);
}

#[tokio::test]
async fn attention_passes_the_same_principal_to_candidate_reads_and_acknowledgments() {
    let (sources, _, owner) = fixture(0);
    let id = rsi_api_protocol::DeviceId::from_bytes([9; 16]);
    let origin = CallOrigin::Device(rsi_api_protocol::AuthenticatedDevice {
        id: id.clone(),
        revoked: tokio_util::sync::CancellationToken::new(),
    });
    let page = owner.read(&origin).await.unwrap();
    finish_mark(owner.mark(
        origin,
        MarkRead {
            host_epoch: page.host_epoch,
            position: page.entries[0].position.clone(),
        },
    ))
    .await
    .unwrap();
    let origins = sources.2.lock().unwrap();
    assert_eq!(origins.len(), 2);
    assert!(
        origins
            .iter()
            .all(|origin| matches!(origin, CallOrigin::Device(device) if device.id == id))
    );
}

async fn finish_mark(admitted: Result<BoxFuture<'static, Result<Position>>>) -> Result<Position> {
    admitted?.await
}
