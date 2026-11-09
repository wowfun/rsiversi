use super::*;
use rsi_navigation_api::{OrderMembership, OrderScope};

async fn account(owner: &Arc<Navigation>) -> Vec<SessionId> {
    let seed = owner
        .order_seed(
            &CallOrigin::Local,
            OrderScope::Coordinates {
                coordinates: ExecutionCoordinates::new(
                    rsi_agent_session_protocol::ExecutionLocation::Local,
                    "/unregistered",
                )
                .unwrap(),
            },
        )
        .unwrap()
        .await
        .unwrap();
    let OrderMembership::Available { mut members, .. } = seed.membership else {
        panic!("bounded fixture");
    };
    members.sort_by_key(|member| member.account_position);
    members.into_iter().map(|member| member.session).collect()
}

#[tokio::test]
async fn workspace_account_moves_survive_activity_partitions_and_restore() {
    let mut owner = owner().await;
    let domain = Arc::new(RecordingDomain(
        owner.domain.spec().clone(),
        Mutex::new(None),
    ));
    let row = WorkspaceRecord::new(
        ExecutionCoordinates::new(
            rsi_agent_session_protocol::ExecutionLocation::Local,
            "/unregistered",
        )
        .unwrap(),
    );
    let workspace = row.id.clone();
    let editable = Arc::get_mut(&mut owner).unwrap();
    editable.domain = domain.clone();
    editable.workspace = Arc::new(Workspaces {
        records: [(workspace.clone(), row)].into(),
        ..Default::default()
    });
    let id = |n| SessionId::new(format!("row-{n:03}")).unwrap();
    let move_to = |session, before| {
        owner
            .insert_workspace_session_before(CallOrigin::Local, workspace.clone(), session, before)
            .unwrap()
    };
    let mut expected = (1..=300).rev().map(id).collect::<Vec<_>>();
    assert_eq!(account(&owner).await, expected);
    assert!(owner.document().workspace_orders.is_empty());
    assert_eq!(
        move_to(id(1), Some(id(299))).await.unwrap(),
        WorkspaceMoveOutcome::Applied
    );
    expected.retain(|entry| entry != &id(1));
    expected.insert(1, id(1));
    assert_eq!(account(&owner).await, expected);
    let persisted = domain.1.lock().unwrap().take().unwrap();
    for (session, before) in [(id(1), Some(id(1))), (id(1), Some(id(299))), (id(2), None)] {
        assert_eq!(
            move_to(session, before).await.unwrap(),
            WorkspaceMoveOutcome::Applied
        );
    }
    assert_eq!(owner.metadata_revision(), 1);
    assert!(domain.1.lock().unwrap().is_none());
    append_member(&owner, 999, "/foreign").await;
    for (session, before) in [(id(999), None), (id(1), Some(id(999))), (id(998), None)] {
        assert_eq!(
            move_to(session, before).await.unwrap(),
            WorkspaceMoveOutcome::InvalidMember
        );
    }
    assert_eq!(owner.metadata_revision(), 1);
    owner
        .store
        .append(AppendBatch {
            session_id: id(2),
            expected_seq: 1,
            header: None,
            facts: vec![Arc::new(
                SessionFact::new(
                    2,
                    10_000,
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
    assert_eq!(account(&owner).await, expected);
    owner
        .set_pin(CallOrigin::Local, id(1), true)
        .unwrap()
        .await
        .unwrap();
    owner
        .archive_intent(CallOrigin::Local, id(299), ArchiveIntent::Archive)
        .unwrap()
        .await
        .unwrap();
    assert_eq!(account(&owner).await, expected);
    let mut restored: Document = serde_json::from_value(persisted).unwrap();
    restored.validate().unwrap();
    owner.state.lock().unwrap().document = Arc::new(restored);
    assert_eq!(account(&owner).await, expected);
    append_member(&owner, 301, "/unregistered").await;
    expected.insert(0, id(301));
    assert_eq!(account(&owner).await, expected);
    move_to(id(300), None).await.unwrap();
    expected.retain(|entry| entry != &id(300));
    expected.push(id(300));
    assert_eq!(account(&owner).await, expected);
    owner.close().await;
}

async fn append_member(owner: &Navigation, number: u64, path: &str) {
    let session = SessionId::new(format!("row-{number:03}")).unwrap();
    let template = owner
        .store
        .header(&SessionId::new("row-001").unwrap())
        .await
        .unwrap();
    let new_header = SessionHeader::new_local(
        session.clone(),
        number,
        path,
        template.agent_preset_id().clone(),
        template.settings().clone(),
    )
    .unwrap();
    owner
        .store
        .append(AppendBatch {
            session_id: session.clone(),
            expected_seq: 0,
            header: Some(new_header),
            facts: vec![Arc::new(
                SessionFact::new(
                    1,
                    number,
                    SessionFactBody::TurnAccepted {
                        turn_id: TurnId::new("turn").unwrap(),
                        text: "new".into(),
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

#[derive(Debug)]
struct AccountDomain {
    recorded: RecordingDomain,
    fail_next: std::sync::atomic::AtomicBool,
}

#[async_trait]
impl Domain for AccountDomain {
    fn ensure_available(&self) -> std::result::Result<(), rsi_storage::StorageError> {
        Ok(())
    }
    fn spec(&self) -> &DomainSpec {
        self.recorded.spec()
    }
    async fn snapshot(
        &self,
    ) -> std::result::Result<BTreeMap<String, serde_json::Value>, rsi_storage::StorageError> {
        self.recorded.snapshot().await
    }
    async fn put(
        &self,
        key: &str,
        value: serde_json::Value,
    ) -> std::result::Result<(), rsi_storage::StorageError> {
        if self
            .fail_next
            .swap(false, std::sync::atomic::Ordering::SeqCst)
        {
            return Err(rsi_storage::StorageError::Io("injected failure".into()));
        }
        self.recorded.put(key, value).await
    }
    async fn delete(&self, key: &str) -> std::result::Result<bool, rsi_storage::StorageError> {
        self.recorded.delete(key).await
    }
}

#[tokio::test]
async fn workspace_account_commit_failure_and_concurrent_moves_preserve_published_state() {
    let mut owner = owner().await;
    let domain = Arc::new(AccountDomain {
        recorded: RecordingDomain(owner.domain.spec().clone(), Mutex::new(None)),
        fail_next: true.into(),
    });
    let row = WorkspaceRecord::new(
        ExecutionCoordinates::new(
            rsi_agent_session_protocol::ExecutionLocation::Local,
            "/unregistered",
        )
        .unwrap(),
    );
    let workspace = row.id.clone();
    let editable = Arc::get_mut(&mut owner).unwrap();
    editable.domain = domain.clone();
    editable.workspace = Arc::new(Workspaces {
        records: [(workspace.clone(), row)].into(),
        ..Default::default()
    });
    let id = |n| SessionId::new(format!("row-{n:03}")).unwrap();
    let move_to = |session, before| {
        owner
            .insert_workspace_session_before(CallOrigin::Local, workspace.clone(), session, before)
            .unwrap()
    };
    let original = account(&owner).await;
    assert!(move_to(id(1), Some(id(300))).await.is_err());
    assert_eq!(account(&owner).await, original);
    assert_eq!(owner.metadata_revision(), 0);
    assert!(domain.recorded.1.lock().unwrap().is_none());

    // Disjoint moves commute; both must survive regardless of writer arrival order.
    let (first, second) =
        tokio::join!(move_to(id(1), Some(id(300))), move_to(id(2), Some(id(299))));
    assert_eq!(first.unwrap(), WorkspaceMoveOutcome::Applied);
    assert_eq!(second.unwrap(), WorkspaceMoveOutcome::Applied);
    assert_eq!(owner.metadata_revision(), 2);
    let mut expected = vec![id(1), id(300), id(2)];
    expected.extend((3..=299).rev().map(id));
    assert_eq!(account(&owner).await, expected);
    let persisted = domain.recorded.1.lock().unwrap().take().unwrap();
    let mut restored: Document = serde_json::from_value(persisted).unwrap();
    restored.validate().unwrap();
    assert_eq!(restored.workspace_orders[&workspace], expected);
    owner.close().await;
}

#[test]
fn workspace_account_document_bounds_and_accounting_match_serialized_bytes() {
    let a = WorkspaceId::from_coordinates(
        &ExecutionCoordinates::new(rsi_agent_session_protocol::ExecutionLocation::Local, "/a")
            .unwrap(),
    );
    let b = WorkspaceId::from_coordinates(
        &ExecutionCoordinates::new(rsi_agent_session_protocol::ExecutionLocation::Local, "/b")
            .unwrap(),
    );
    let id = SessionId::new("only").unwrap();
    let mut document = Document::default();
    for ids in [
        vec![id.clone()],
        vec![id.clone(), SessionId::new("second").unwrap()],
        vec![],
    ] {
        document.edit_order(&a, ids).unwrap();
        let accounting = document.accounting.unwrap();
        assert_eq!(
            accounting.orders.bytes(),
            serde_json::to_vec(&document.workspace_orders)
                .unwrap()
                .len()
        );
        assert_eq!(
            accounting.order_members,
            document
                .workspace_orders
                .values()
                .map(Vec::len)
                .sum::<usize>()
        );
        assert_eq!(
            serde_json::to_vec(&serde_json::json!({"metadata":&document}))
                .unwrap()
                .len(),
            accounting.bytes(document.revision)
        );
    }
    for orders in [
        [(a.clone(), vec![id.clone(), id.clone()])].into(),
        [(a.clone(), vec![id.clone()]), (b, vec![id.clone()])].into(),
        [(a.clone(), vec![])].into(),
        [(
            a,
            (0..1025)
                .map(|n| SessionId::new(format!("{n}")).unwrap())
                .collect(),
        )]
        .into(),
    ] {
        document.workspace_orders = orders;
        assert!(document.validate().is_err());
    }
}
