use super::*;
use rsi_agent_turn_protocol::{
    ResidentActivity, ResidentActivityPage, ResidentComposition, SessionProjectionChanges,
    SessionProjections,
};
use rsi_session_protocol::SessionIngress as _;
use rsi_session_protocol::{ActivityRequest, ActivityStatus};

#[derive(Debug)]
struct ReadingPolicy {
    lease: CancellationToken,
    reads: AtomicUsize,
}
impl rsi_session_protocol::SessionProtection for ReadingPolicy {
    fn view(
        &self,
        _: &rsi_agent_session_protocol::SessionProtectionScope,
        _: &rsi_api_protocol::CallOrigin,
    ) -> rsi_session_protocol::Result<CancellationToken> {
        self.reads.fetch_add(1, Ordering::SeqCst);
        if self.lease.is_cancelled() {
            Err(SessionError::Api(rsi_api_protocol::ApiError::Unauthorized))
        } else {
            Ok(self.lease.clone())
        }
    }
}

#[tokio::test]
async fn activity_revocation_during_collection_refuses_protected_and_device_delivery() {
    for revoke_scope in [true, false] {
        let store = Arc::new(MemoryStore::new());
        let id = SessionId::new("protected-activity").unwrap();
        let header = SessionHeader::new_local(
            id.clone(),
            1,
            std::env::current_dir().unwrap().to_str().unwrap(),
            AgentPresetId::new("removed").unwrap(),
            test_settings(),
        )
        .unwrap()
        .with_protection(
            rsi_agent_session_protocol::SessionProtectionScope::new("fixture", "activity").unwrap(),
        )
        .unwrap();
        store
            .append(AppendBatch {
                session_id: id.clone(),
                expected_seq: 0,
                header: Some(header),
                facts: vec![
                    SessionFact::new(
                        1,
                        1,
                        SessionFactBody::TurnAccepted {
                            reasoning_effort: None,
                            turn_id: TurnId::new("protected-turn").unwrap(),
                            text: "protected activity".into(),
                            model: None,
                            sandbox: SandboxMode::WorkspaceWrite,
                            require_approval: false,
                        },
                    )
                    .unwrap()
                    .into(),
                ],
            })
            .await
            .unwrap();
        let roster = Arc::new(Roster {
            ids: vec![id],
            present: std::sync::atomic::AtomicBool::new(true),
            revision: std::sync::atomic::AtomicU64::new(0),
        });
        let approvals = Arc::new(TreeApprovals::default());
        let gate = approvals.pending.lock().await;
        let policy = Arc::new(ReadingPolicy {
            lease: CancellationToken::new(),
            reads: AtomicUsize::new(0),
        });
        let device_stop = CancellationToken::new();
        let service = LocalSessionService::new(
            rsi_meta::Execution::native(tokio::runtime::Handle::current()),
            Arc::new(UnavailableCommands),
            roster,
            Arc::new(UnavailableTurns::default()),
            store,
            rsi_agent_context::ContextBudget::default(),
            Arc::new(UnavailableComposition),
            Arc::new(UnavailableWorkspace),
            Arc::new(UnavailableSettings),
            Arc::new(UnavailableLanguage),
            Arc::new(UnavailableImage),
            Arc::new(UnavailableMedia),
            approvals.clone(),
        )
        .with_protection(policy.clone());
        let scoped = service.scoped(rsi_api_protocol::CallOrigin::Device(
            rsi_api_protocol::AuthenticatedDevice {
                id: rsi_api_protocol::DeviceId::from_bytes([1; 16]),
                revoked: device_stop.clone(),
            },
        ));
        let mut reading = Box::pin(scoped.activity());
        assert!(futures_util::poll!(&mut reading).is_pending());
        assert_eq!(
            (
                policy.reads.load(Ordering::SeqCst),
                approvals.reads.load(Ordering::SeqCst)
            ),
            (1, 1),
            "collection is blocked after authorization"
        );
        if revoke_scope {
            policy.lease.cancel();
        } else {
            device_stop.cancel();
        }
        drop(gate);
        let reply = reading.await;
        service.stop().await.unwrap();
        assert!(
            matches!(
                reply,
                Err(SessionError::Api(rsi_api_protocol::ApiError::Unauthorized))
            ),
            "revoked page leaked: {reply:?}"
        );
    }
}

#[derive(Debug)]
struct Roster {
    ids: Vec<SessionId>,
    present: std::sync::atomic::AtomicBool,
    revision: std::sync::atomic::AtomicU64,
}

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "One maximum-roster fixture verifies batching, cache refresh, revision races and batch failure recovery"
)]
async fn maximum_activity_roster_uses_one_access_and_two_cut_batches() {
    let store = Arc::new(MemoryStore::new());
    let ids = (0..128)
        .map(|index| SessionId::new(format!("activity-{index:03}")).unwrap())
        .collect::<Vec<_>>();
    for id in &ids {
        let header = SessionHeader::new_local(
            id.clone(),
            1,
            std::env::current_dir().unwrap().to_str().unwrap(),
            AgentPresetId::new("removed-preset").unwrap(),
            test_settings(),
        )
        .unwrap();
        store
            .append(AppendBatch {
                session_id: id.clone(),
                expected_seq: 0,
                header: Some(header),
                facts: vec![Arc::new(
                    SessionFact::new(
                        1,
                        1,
                        SessionFactBody::TurnAccepted {
                            reasoning_effort: None,
                            turn_id: TurnId::new("turn").unwrap(),
                            text: "accepted".into(),
                            model: None,
                            sandbox: SandboxMode::WorkspaceWrite,
                            require_approval: false,
                        },
                    )
                    .unwrap(),
                )],
            })
            .await
            .unwrap();
    }
    let roster = Arc::new(Roster {
        ids: ids[64..].to_vec(),
        present: std::sync::atomic::AtomicBool::new(true),
        revision: std::sync::atomic::AtomicU64::new(0),
    });
    let service = LocalSessionService::new(
        rsi_meta::Execution::native(tokio::runtime::Handle::current()),
        Arc::new(UnavailableCommands),
        roster.clone(),
        Arc::new(UnavailableTurns::default()),
        store.clone(),
        rsi_agent_context::ContextBudget::default(),
        Arc::new(UnavailableComposition),
        Arc::new(UnavailableWorkspace),
        Arc::new(UnavailableSettings),
        Arc::new(UnavailableLanguage),
        Arc::new(UnavailableImage),
        Arc::new(UnavailableMedia),
        Arc::new(TreeApprovals::default()),
    );
    for id in &ids[..64] {
        drop(service.attach(id).await.unwrap());
    }
    let before = store.activity_read_counts();
    let open_reads = store.open_turn_read_count();
    let page = service.activity().await.unwrap();
    assert_eq!(page.entries.len(), 128);
    let after = store.activity_read_counts();
    assert_eq!(
        (after.0 - before.0, after.1 - before.1, after.2 - before.2),
        (2, 1, 0)
    );
    assert_eq!(store.open_turn_read_count(), open_reads);
    service.activity().await.unwrap();
    assert_eq!(store.activity_read_counts().0, after.0);
    roster.revision.fetch_add(1, Ordering::AcqRel);
    service.activity().await.unwrap();
    assert_eq!(store.activity_read_counts().0, after.0 + 2);
    service.activity().await.unwrap();
    assert_eq!(store.activity_read_counts().0, after.0 + 2);
    roster.revision.fetch_add(1, Ordering::AcqRel);
    let (entered, release) = store.pause_next_activity_cuts();
    let mut reading = Box::pin(service.activity());
    assert!(futures_util::poll!(reading.as_mut()).is_pending());
    entered.await.unwrap();
    roster.revision.fetch_add(1, Ordering::AcqRel);
    release.send(()).unwrap();
    reading.await.unwrap();
    let raced = store.activity_read_counts().0;
    service.activity().await.unwrap();
    assert_eq!(
        store.activity_read_counts().0,
        raced + 2,
        "a revision race must not populate cut cache"
    );
    roster.revision.fetch_add(1, Ordering::AcqRel);
    store.fail_next_activity_cuts(rsi_agent_store_protocol::StoreError::Io(
        "injected cut failure".into(),
    ));
    let failed = service.activity().await.unwrap();
    assert_eq!(
        failed
            .entries
            .iter()
            .filter(|entry| entry.fact_seq == "0" && entry.status == ActivityStatus::Unknown)
            .count(),
        64
    );
    assert!(
        service
            .activity()
            .await
            .unwrap()
            .entries
            .iter()
            .all(|entry| entry.fact_seq == "1")
    );
    service.stop().await.unwrap();
}
#[async_trait]
impl SessionProjections for Roster {
    fn activity_revision(&self) -> Option<u64> {
        Some(self.revision.load(Ordering::Acquire))
    }
    fn resident_activity(&self) -> TurnResult<ResidentActivityPage> {
        Ok(ResidentActivityPage {
            entries: if self.present.load(Ordering::SeqCst) {
                self.ids
                    .iter()
                    .map(|id| ResidentActivity {
                        session: id.clone(),
                        running: true,
                    })
                    .collect()
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
    let cwd = rsi_workspace_protocol::canonical_native_path(
        std::env::current_dir().unwrap().canonicalize().unwrap(),
    );
    for session in [&id, &abandoned] {
        let header = SessionHeader::new_local(
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
        ids: vec![id.clone()],
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
        rsi_agent_context::ContextBudget::default(),
        Arc::new(UnavailableComposition),
        Arc::new(UnavailableWorkspace),
        Arc::new(UnavailableSettings),
        Arc::new(UnavailableLanguage),
        Arc::new(UnavailableImage),
        Arc::new(UnavailableMedia),
        approvals.clone(),
    );
    let mut page = service.activity().await.unwrap();
    let reads = store.activity_read_counts().0;
    for _ in 0..16 {
        service.activity().await.unwrap();
    }
    assert_eq!(
        store.activity_read_counts().0,
        reads,
        "idle attention must not revisit Store metadata"
    );
    roster.revision.fetch_add(1, Ordering::AcqRel);
    service.activity().await.unwrap();
    assert_eq!(
        store.activity_read_counts().0,
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

    rsi_agent_testkit::append_history_fixture(
        store.as_ref(),
        AppendBatch {
            session_id: id.clone(),
            expected_seq: 1,
            header: None,
            facts: vec![Arc::new(
                SessionFact::new(
                    2,
                    2,
                    SessionFactBody::TurnTerminal {
                        turn_id: TurnId::new("exact-turn").unwrap(),
                        outcome: rsi_agent_session_protocol::TurnOutcome::Completed,
                        result: None,
                    },
                )
                .unwrap(),
            )],
        },
    )
    .await
    .unwrap();
    roster.present.store(true, Ordering::SeqCst);
    roster.revision.fetch_add(1, Ordering::AcqRel);
    page = service.activity().await.unwrap();
    let completed = page.entries.iter().find(|row| row.session == id).unwrap();
    assert_eq!(completed.fact_seq, "2");
    assert_eq!(
        completed.status,
        ActivityStatus::Idle,
        "a closed durable cut takes precedence over a running resident sample"
    );
    assert!(completed.requests.is_empty());
    assert!(!page.truncated);
}
