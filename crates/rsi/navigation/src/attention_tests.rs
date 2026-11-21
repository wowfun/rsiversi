use super::*;
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
    async fn device_drafts(
        &self,
        _: &rsi_api_protocol::AuthenticatedDevice,
    ) -> rsi_session_protocol::Result<Vec<rsi_session_protocol::DeviceDraft>> {
        Ok(Vec::new())
    }
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
    async fn read_summary(
        &self,
        _: &rsi_agent_session_protocol::SessionId,
    ) -> rsi_session_protocol::Result<rsi_session_protocol::SessionSummary> {
        Err(rsi_session_protocol::SessionError::Api(
            ApiError::Unauthorized,
        ))
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
    async fn visible_activity_sessions(
        &self,
        ids: &[rsi_agent_session_protocol::SessionId],
    ) -> rsi_session_protocol::Result<Vec<rsi_agent_session_protocol::SessionId>> {
        Ok(ids
            .iter()
            .filter(|id| id.as_str() == "visible")
            .cloned()
            .collect())
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
fn fixture() -> Attention {
    let sources = Arc::new(Sources(
        Arc::new(Mutex::new(ExternalStatus::Ready)),
        Arc::default(),
        Arc::default(),
    ));
    Attention {
        session: sources.clone(),
        external: sources,
        epoch: HostEpoch::generate().unwrap(),
        records: Arc::default(),
        admission: Admission::new(
            2,
            rsi_meta::Execution::native(tokio::runtime::Handle::current()),
        ),
    }
}

#[test]
fn reminders_preserve_manual_intent_and_exact_cuts() {
    use rsi_agent_session_protocol::SessionId;
    let records = Records::default();
    let session = SessionId::new("visible").unwrap();
    let id = ConversationIdentity::Native(session.clone());
    for (sequence, failed) in [(8, false), (12, true), (8, false)] {
        records.handle(&SessionActivityEvent::TurnEnded {
            session: session.clone(),
            sequence,
            failed,
        });
    }
    {
        let mut rows = records.0.lock().unwrap();
        let record = rows.get_mut(&id).unwrap();
        assert_eq!(record.completed, 12);
        record.mark(ReadAction::Unread, 0);
        record.mark(ReadAction::Acknowledge, 12);
        assert_eq!(
            record.reading(),
            Reading {
                unseen: false,
                manual: true,
                failed: true
            }
        );
        record.mark(ReadAction::Read, 8);
        assert!(!record.reading().unread());
        assert_eq!(record.read, 12);
    }
    records.handle(&SessionActivityEvent::Retired(session));
    assert!(records.0.lock().unwrap().is_empty());
}

#[tokio::test]
async fn cold_reminders_are_filtered_at_the_callers_session_boundary() {
    let owner = fixture();
    for id in ["visible", "private"] {
        owner.records.handle(&SessionActivityEvent::TurnEnded {
            session: rsi_agent_session_protocol::SessionId::new(id).unwrap(),
            sequence: 12,
            failed: false,
        });
    }
    let origin = CallOrigin::Device(rsi_api_protocol::AuthenticatedDevice {
        id: rsi_api_protocol::DeviceId::from_bytes([9; 16]),
        revoked: tokio_util::sync::CancellationToken::new(),
    });
    let page = owner.read(&origin).await.unwrap();
    page.validate().unwrap();
    let native = page
        .entries
        .iter()
        .filter(|entry| matches!(entry.position.conversation, ConversationIdentity::Native(_)))
        .collect::<Vec<_>>();
    assert_eq!(native.len(), 1);
    assert_eq!(
        native[0].position.conversation,
        ConversationIdentity::Native(
            rsi_agent_session_protocol::SessionId::new("visible").unwrap()
        )
    );
    assert!(native[0].reading.unseen);
    assert!(!page.truncated);
    let request = Mark {
        host_epoch: owner.epoch.clone(),
        position: Position {
            conversation: ConversationIdentity::Native(
                rsi_agent_session_protocol::SessionId::new("private").unwrap(),
            ),
            epoch: "0".into(),
            sequence: "12".into(),
        },
        action: ReadAction::Read,
    };
    assert!(matches!(
        owner.mark(origin, request).await,
        Err(ApiError::Unauthorized)
    ));
}

#[tokio::test]
async fn external_marks_are_shared_and_keep_their_epoch_fence() {
    let owner = fixture();
    let row = owner
        .read(&CallOrigin::Local)
        .await
        .unwrap()
        .entries
        .remove(0);
    let device = CallOrigin::Device(rsi_api_protocol::AuthenticatedDevice {
        id: rsi_api_protocol::DeviceId::from_bytes([1; 16]),
        revoked: tokio_util::sync::CancellationToken::new(),
    });
    let mut mark = Mark {
        host_epoch: owner.epoch.clone(),
        action: ReadAction::Read,
        position: row.position,
    };
    owner.mark(device.clone(), mark.clone()).await.unwrap();
    assert!(
        owner
            .read(&CallOrigin::Local)
            .await
            .unwrap()
            .entries
            .is_empty()
    );
    mark.action = ReadAction::Unread;
    owner.mark(CallOrigin::Local, mark.clone()).await.unwrap();
    assert!(owner.read(&device).await.unwrap().entries[0].reading.manual);
    mark.position.epoch = "3".into();
    assert!(matches!(
        owner.mark(device, mark).await,
        Err(ApiError::Invalid(_))
    ));
}
