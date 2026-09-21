use super::*;
use rsi_agent_turn_protocol::{
    ResidentActivity, ResidentActivityPage, ResidentComposition, SessionProjectionChanges,
    SessionProjections,
};
use rsi_session_protocol::{ActivityRequest, ActivityStatus};
#[derive(Debug)]
struct Roster {
    id: SessionId,
    present: std::sync::atomic::AtomicBool,
    revision: std::sync::atomic::AtomicU64,
}
#[async_trait]
impl SessionProjections for Roster {
    fn activity_revision(&self) -> Option<u64> {
        Some(self.revision.load(Ordering::Acquire))
    }
    fn resident_activity(&self) -> TurnResult<ResidentActivityPage> {
        Ok(ResidentActivityPage {
            entries: if self.present.load(Ordering::SeqCst) {
                vec![ResidentActivity {
                    session: self.id.clone(),
                    running: true,
                }]
            } else {
                vec![]
            },
            has_more: false,
        })
    }
    fn resident_composition(&self, _: &SessionId) -> TurnResult<ResidentComposition> {
        panic!("attention cannot resolve a composition")
    }
    fn watch_projection_changes(&self, _: &SessionId) -> TurnResult<SessionProjectionChanges> {
        panic!("attention cannot create a session observer")
    }
    async fn projection_snapshot(
        &self,
        _: &SessionId,
    ) -> TurnResult<rsi_agent_session_protocol::SessionProjectionSnapshot> {
        panic!("attention cannot hydrate history")
    }
}
#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "One causal owner-loss scenario verifies discovery, exact targets and absence of hydration"
)]
async fn activity_distinguishes_current_owner_from_durable_open_turn_and_bounds_exact_targets() {
    let store = Arc::new(MemoryStore::new());
    let id = SessionId::new("attention-running").unwrap();
    let abandoned = SessionId::new("attention-abandoned").unwrap();
    let cwd = std::env::current_dir().unwrap().canonicalize().unwrap();
    for session in [&id, &abandoned] {
        let header = SessionHeader::new(
            session.clone(),
            1,
            cwd.to_str().unwrap(),
            AgentPresetId::new("removed-preset").unwrap(),
            test_settings(),
        )
        .unwrap();
        store
            .append(AppendBatch {
                session_id: session.clone(),
                expected_seq: 0,
                header: Some(header),
                facts: vec![Arc::new(
                    SessionFact::new(
                        1,
                        1,
                        SessionFactBody::TurnAccepted {
                            reasoning_effort: None,
                            turn_id: TurnId::new("exact-turn").unwrap(),
                            text: "accepted".into(),
                            model: None,
                            sandbox: SandboxMode::WorkspaceWrite,
                            require_approval: true,
                        },
                    )
                    .unwrap(),
                )],
            })
            .await
            .unwrap();
    }
    let roster = Arc::new(Roster {
        id: id.clone(),
        present: std::sync::atomic::AtomicBool::new(true),
        revision: std::sync::atomic::AtomicU64::new(0),
    });
    let approvals = Arc::new(TreeApprovals::default());
    approvals.pending.lock().await.insert(
        id.clone(),
        (0..33)
            .map(|index| ApprovalRequest {
                review: None,
                subject: ApprovalSubject::new(id.as_str(), "exact-turn", format!("effect-{index}"))
                    .unwrap(),
                id: format!("approval-{index}"),
                action: "write".into(),
                reason: "bounded attention".into(),
            })
            .collect(),
    );
    let service = LocalSessionService::new(
        rsi_meta::Execution::native(tokio::runtime::Handle::current()),
        Arc::new(UnavailableCommands),
        roster.clone(),
        Arc::new(UnavailableTurns::default()),
        store.clone(),
        Arc::new(UnavailableComposition),
        Arc::new(UnavailableWorkspace),
        Arc::new(UnavailableSettings),
        Arc::new(UnavailableLanguage),
        Arc::new(UnavailableImage),
        Arc::new(UnavailableMedia),
        approvals.clone(),
    );
    let mut page = service.activity().await.unwrap();
    let reads = store.open_turn_read_count();
    for _ in 0..16 {
        service.activity().await.unwrap();
    }
    assert_eq!(
        store.open_turn_read_count(),
        reads,
        "idle attention must not revisit Store metadata"
    );
    roster.revision.fetch_add(1, Ordering::AcqRel);
    service.activity().await.unwrap();
    assert_eq!(
        store.open_turn_read_count(),
        reads + 1,
        "a changed commit revision invalidates the durable cut"
    );

    assert_eq!(
        page.entries.len(),
        1,
        "Only the actual resident is discovered"
    );
    assert_eq!(page.entries[0].status, ActivityStatus::Running);
    assert_eq!(page.entries[0].requests.len(), 32);
    assert!(page.truncated);
    assert_eq!(
        page.entries[0].requests[0],
        ActivityRequest::Approval {
            turn: TurnId::new("exact-turn").unwrap(),
            request: "approval-0".into()
        }
    );
    drop(service.attach(&abandoned).await.unwrap());
    drop(service.attach(&id).await.unwrap());
    page = service.activity().await.unwrap();
    assert_eq!(
        page.entries
            .iter()
            .find(|row| row.session == abandoned)
            .unwrap()
            .status,
        ActivityStatus::Unknown
    );
    roster.present.store(false, Ordering::SeqCst);
    approvals.pending.lock().await.clear();
    page = service.activity().await.unwrap();
    assert!(
        page.entries
            .iter()
            .all(|row| row.status == ActivityStatus::Unknown && row.requests.is_empty())
    );
    assert!(!page.truncated);
}
