use super::*;
use rsi_navigation_api::{OrderMembership, OrderScope};

#[tokio::test]
async fn attachment_enrolls_only_the_named_session_once() {
    let mut owner = owner().await;
    let record = workspace_record(
        ExecutionCoordinates::new(
            rsi_agent_session_protocol::ExecutionLocation::Local,
            "/unregistered",
        )
        .unwrap(),
    );
    let workspace = record.id.clone();
    let other = workspace_record(
        ExecutionCoordinates::new(
            rsi_agent_session_protocol::ExecutionLocation::Local,
            "/other",
        )
        .unwrap(),
    );
    let other_id = other.id.clone();
    Arc::get_mut(&mut owner).unwrap().workspace = Arc::new(Workspaces {
        records: [
            (workspace.clone(), record.clone()),
            (other_id.clone(), other.clone()),
        ]
        .into(),
        ..Default::default()
    });
    let first = SessionId::new("row-001").unwrap();
    let sibling = SessionId::new("row-002").unwrap();
    assert!(!owner.workspace_contains(&workspace, &first));
    let mut directory = owner.metadata_changes();
    let archive = owner.archive_changes();
    for first_attachment in [true, false] {
        owner
            .attach_workspace_session(CallOrigin::Local, workspace.clone(), first.clone())
            .unwrap()
            .await
            .unwrap();
        assert!(owner.workspace_contains(&workspace, &first));
        assert_eq!(owner.document().revision, 1);
        assert_eq!(directory.has_changed().unwrap(), first_attachment);
        directory.borrow_and_update();
        assert!(!archive.has_changed().unwrap());
    }
    assert!(!owner.workspace_contains(&workspace, &sibling));
    assert!(
        owner
            .attach_workspace_session(CallOrigin::Local, other_id.clone(), first.clone())
            .unwrap()
            .await
            .is_err()
    );
    assert!(!owner.workspace_contains(&other_id, &first));
    let (members, changed_at) = owner
        .workspace_members(CallOrigin::Local, &record)
        .unwrap()
        .await
        .unwrap();
    let summary = owner.store.session_summary(&first).await.unwrap();
    let detached = owner.store.session_summary(&sibling).await.unwrap();
    let projected = owner
        .workspace_catalog_metadata(&[detached, summary.clone()])
        .await
        .unwrap();
    assert_eq!(projected[0].account, None);
    assert_eq!(projected[1].account, Some((workspace.clone(), 0)));
    assert_eq!(members, [first]);
    assert!(changed_at.is_some());
    assert!(
        owner
            .workspace_members(CallOrigin::Local, &other)
            .unwrap()
            .await
            .unwrap()
            .0
            .is_empty()
    );
    assert_eq!(owner.document().revision, 1);
    owner.close().await;
}

async fn seed_members(
    owner: &Navigation,
    domain: &TestDomain,
    workspace: &WorkspaceId,
    members: Vec<SessionId>,
) {
    let mut document = Document::default();
    document.workspace_orders.insert(
        workspace.clone(),
        WorkspaceAccount {
            updated_at_ms: 1,
            members,
        },
    );
    document.validate().unwrap();
    domain.seed_document(&document).await;
    owner.state.lock().unwrap().document = Arc::new(document);
}

#[tokio::test]
async fn purging_the_last_member_keeps_the_workspace_clock_after_reopen() {
    use rsi_agent_store_protocol::StoreMaintenance;
    use rsi_session_protocol::SessionPurgeCleanup;
    let root = tempfile::tempdir().unwrap();
    let store = Arc::new(rsi_agent_store_sqlite::SqliteStore::open(root.path()).unwrap());
    let mut navigation = owner().await;
    let member = SessionId::new("row-001").unwrap();
    let header = navigation.store.header(&member).await.unwrap();
    let workspace = fixture_workspace_id(header.coordinates());
    let request = seed_purge_cleanup(&store, header).await;
    let domain = TestDomain::new(navigation.domain.spec().clone()).await;
    let owner = Arc::get_mut(&mut navigation).unwrap();
    owner.domain = domain.domain.clone();
    owner.store = store.clone();
    seed_members(&navigation, &domain, &workspace, vec![member.clone()]).await;
    let cleanup = PurgeCleanup(Arc::downgrade(&navigation));
    store.begin_purge(request).await.unwrap().unwrap();
    assert!(store.purge_batch("cleanup").await.unwrap().complete);
    cleanup.cleanup("cleanup").await.unwrap();
    let changed = navigation.workspace_changed_at(&workspace).unwrap();
    assert!(changed > 1);
    cleanup.cleanup("cleanup").await.unwrap();
    assert_eq!(domain.writes(), 1);
    let reopened = load_document(&domain.domain).unwrap();
    assert_eq!(reopened.workspace_orders[&workspace].updated_at_ms, changed);
    assert!(reopened.workspace_orders[&workspace].members.is_empty());
    assert!(
        !serde_json::to_string(&reopened)
            .unwrap()
            .contains(member.as_str())
    );
    assert_eq!(
        navigation.workspace_changed_at(&WorkspaceId::from_bytes([9; 32])),
        None
    );
}

#[tokio::test]
async fn remount_discards_retired_accounts_and_preserves_registered_empty_clocks() {
    use rsi_workspace_protocol::WorkspaceRegistrationCleanup;
    let mut navigation = owner().await;
    let live = workspace_record(
        ExecutionCoordinates::new(
            rsi_agent_session_protocol::ExecutionLocation::Local,
            "/live",
        )
        .unwrap(),
    );
    let retired = WorkspaceId::from_bytes([7; 32]);
    let domain = TestDomain::new(navigation.domain.spec().clone()).await;
    let owner = Arc::get_mut(&mut navigation).unwrap();
    owner.workspace = Arc::new(Workspaces {
        records: [(live.id.clone(), live.clone())].into(),
        ..Default::default()
    });
    owner.domain = domain.domain.clone();
    let mut saved = Document::default();
    for id in [&live.id, &retired] {
        saved.workspace_orders.insert(
            id.clone(),
            WorkspaceAccount {
                updated_at_ms: 9,
                members: vec![],
            },
        );
    }
    saved.validate().unwrap();
    domain.seed_document(&saved).await;
    owner.state.lock().unwrap().document = Arc::new(saved);
    navigation.reconcile_accounts().await.unwrap();
    let reopened = load_document(&domain.domain).unwrap();
    assert_eq!(reopened.workspace_orders.len(), 1);
    assert_eq!(reopened.workspace_orders[&live.id].updated_at_ms, 9);
    let cleanup = PurgeCleanup(Arc::downgrade(&navigation));
    cleanup.forget(&retired).await.unwrap();
    assert_eq!(domain.writes(), 1);
    cleanup.forget(&live.id).await.unwrap();
    assert!(
        load_document(&domain.domain)
            .unwrap()
            .workspace_orders
            .is_empty()
    );
}

async fn account(owner: &Arc<Navigation>, workspace: &WorkspaceId) -> Vec<SessionId> {
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
    let OrderMembership::Available {
        groups,
        mut members,
    } = seed.membership
    else {
        panic!("bounded fixture");
    };
    members
        .retain(|member| groups[usize::from(member.group)].workspace.as_ref() == Some(workspace));
    members.sort_by_key(|member| member.account_position);
    members.into_iter().map(|member| member.session).collect()
}

#[tokio::test]
async fn workspace_account_moves_survive_activity_partitions_and_restore() {
    let mut owner = owner().await;
    let domain = TestDomain::new(owner.domain.spec().clone()).await;
    let row = workspace_record(
        ExecutionCoordinates::new(
            rsi_agent_session_protocol::ExecutionLocation::Local,
            "/unregistered",
        )
        .unwrap(),
    );
    let workspace = row.id.clone();
    let editable = Arc::get_mut(&mut owner).unwrap();
    editable.domain = domain.domain.clone();
    editable.workspace = Arc::new(Workspaces {
        records: [(workspace.clone(), row.clone())].into(),
        ..Default::default()
    });
    let id = |n| SessionId::new(format!("row-{n:03}")).unwrap();
    let move_to = |session, before| {
        owner
            .insert_workspace_session_before(CallOrigin::Local, workspace.clone(), session, before)
            .unwrap()
    };
    let mut expected = (1..=300).rev().map(id).collect::<Vec<_>>();
    seed_members(&owner, &domain, &workspace, expected.clone()).await;
    assert_eq!(account(&owner, &workspace).await, expected);
    assert_eq!(
        move_to(id(1), Some(id(299))).await.unwrap(),
        WorkspaceMoveOutcome::Applied(row.clone())
    );
    expected.retain(|entry| entry != &id(1));
    expected.insert(1, id(1));
    assert_eq!(account(&owner, &workspace).await, expected);
    let persisted = domain.take_document().unwrap();
    for (session, before) in [(id(1), Some(id(1))), (id(1), Some(id(299))), (id(2), None)] {
        assert_eq!(
            move_to(session, before).await.unwrap(),
            WorkspaceMoveOutcome::Applied(row.clone())
        );
    }
    assert_eq!(owner.document().revision, 1);
    assert!(domain.document().is_none());
    append_member(&owner, 999, "/foreign").await;
    for (session, before) in [(id(999), None), (id(1), Some(id(999))), (id(998), None)] {
        assert_eq!(
            move_to(session, before).await.unwrap(),
            WorkspaceMoveOutcome::InvalidMember
        );
    }
    assert_eq!(owner.document().revision, 1);
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
    assert_eq!(account(&owner, &workspace).await, expected);
    owner
        .set_pin(CallOrigin::Local, id(1), true)
        .unwrap()
        .await
        .unwrap();
    finish_fixture_turn(&owner, &id(299)).await;
    owner
        .archive_intent(CallOrigin::Local, id(299), ArchiveIntent::Archive)
        .unwrap()
        .await
        .unwrap();
    assert_eq!(account(&owner, &workspace).await, expected);
    let mut restored: Document = serde_json::from_value(persisted).unwrap();
    restored.validate().unwrap();
    owner.state.lock().unwrap().document = Arc::new(restored);
    assert_eq!(account(&owner, &workspace).await, expected);
    append_member(&owner, 301, "/unregistered").await;
    assert_eq!(account(&owner, &workspace).await, expected);
    owner
        .attach_workspace_session(CallOrigin::Local, workspace.clone(), id(301))
        .unwrap()
        .await
        .unwrap();
    expected.insert(0, id(301));
    assert_eq!(account(&owner, &workspace).await, expected);
    move_to(id(300), None).await.unwrap();
    expected.retain(|entry| entry != &id(300));
    expected.push(id(300));
    assert_eq!(account(&owner, &workspace).await, expected);
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

#[tokio::test]
async fn workspace_account_commit_failure_and_concurrent_moves_preserve_published_state() {
    let mut owner = owner().await;
    let domain = TestDomain::new(owner.domain.spec().clone()).await;
    let row = workspace_record(
        ExecutionCoordinates::new(
            rsi_agent_session_protocol::ExecutionLocation::Local,
            "/unregistered",
        )
        .unwrap(),
    );
    let workspace = row.id.clone();
    let editable = Arc::get_mut(&mut owner).unwrap();
    editable.domain = domain.domain.clone();
    editable.workspace = Arc::new(Workspaces {
        records: [(workspace.clone(), row.clone())].into(),
        ..Default::default()
    });
    let id = |n| SessionId::new(format!("row-{n:03}")).unwrap();
    let move_to = |session, before| {
        owner
            .insert_workspace_session_before(CallOrigin::Local, workspace.clone(), session, before)
            .unwrap()
    };
    seed_members(
        &owner,
        &domain,
        &workspace,
        (1..=300).rev().map(id).collect(),
    )
    .await;
    domain
        .backend
        .state
        .reject_write
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let original = account(&owner, &workspace).await;
    assert!(move_to(id(1), Some(id(300))).await.is_err());
    assert_eq!(account(&owner, &workspace).await, original);
    assert_eq!(owner.document().revision, 0);
    assert!(domain.document().is_none());

    domain
        .backend
        .state
        .reject_write
        .store(false, std::sync::atomic::Ordering::SeqCst);
    // Disjoint moves commute; both must survive regardless of writer arrival order.
    let (first, second) =
        tokio::join!(move_to(id(1), Some(id(300))), move_to(id(2), Some(id(299))));
    assert_eq!(first.unwrap(), WorkspaceMoveOutcome::Applied(row.clone()));
    assert_eq!(second.unwrap(), WorkspaceMoveOutcome::Applied(row));
    assert_eq!(owner.document().revision, 2);
    let mut expected = vec![id(1), id(300), id(2)];
    expected.extend((3..=299).rev().map(id));
    assert_eq!(account(&owner, &workspace).await, expected);
    let persisted = domain.take_document().unwrap();
    let mut restored: Document = serde_json::from_value(persisted).unwrap();
    restored.validate().unwrap();
    assert_eq!(restored.workspace_orders[&workspace].members, expected);
    owner.close().await;
}

#[test]
fn workspace_account_document_bounds_and_accounting_match_serialized_bytes() {
    let a = fixture_workspace_id(
        &ExecutionCoordinates::new(rsi_agent_session_protocol::ExecutionLocation::Local, "/a")
            .unwrap(),
    );
    let b = fixture_workspace_id(
        &ExecutionCoordinates::new(rsi_agent_session_protocol::ExecutionLocation::Local, "/b")
            .unwrap(),
    );
    let id = SessionId::new("only").unwrap();
    let mut document = Document::default();
    for ids in [
        vec![id.clone()],
        vec![
            id.clone(),
            SessionId::new("AZaz09._-:").unwrap(),
            SessionId::new("x".repeat(rsi_agent_session_protocol::MAXIMUM_AGENT_IDENTIFIER_BYTES))
                .unwrap(),
        ],
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
                .map(|row| row.members.len())
                .sum::<usize>()
        );
        assert_eq!(
            serde_json::to_vec(&serde_json::json!({"metadata":&document}))
                .unwrap()
                .len(),
            accounting.bytes(document.revision)
        );
    }
    let row = |members| WorkspaceAccount {
        updated_at_ms: 42,
        members,
    };
    document.workspace_orders = [(
        a.clone(),
        row((0..1025)
            .map(|n| SessionId::new(format!("{n}")).unwrap())
            .collect()),
    )]
    .into();
    document.validate().unwrap();
    for orders in [
        [(a.clone(), row(vec![id.clone(), id.clone()]))].into(),
        [
            (a.clone(), row(vec![id.clone()])),
            (b, row(vec![id.clone()])),
        ]
        .into(),
        [(
            a.clone(),
            WorkspaceAccount {
                updated_at_ms: u64::MAX,
                members: vec![],
            },
        )]
        .into(),
        [(
            a,
            row((0..8193)
                .map(|n| SessionId::new(format!("{n}")).unwrap())
                .collect()),
        )]
        .into(),
    ] {
        document.workspace_orders = orders;
        assert!(document.validate().is_err());
    }
}

#[tokio::test]
async fn recreated_workspace_cannot_inherit_or_duplicate_deleted_account() {
    let mut old = owner().await;
    let old_domain = TestDomain::new(old.domain.spec().clone()).await;
    Arc::get_mut(&mut old).unwrap().domain = old_domain.domain.clone();
    let mut record = workspace_record(
        ExecutionCoordinates::new(
            rsi_agent_session_protocol::ExecutionLocation::Local,
            "/unregistered",
        )
        .unwrap(),
    );
    let old_id = record.id.clone();
    Arc::get_mut(&mut old).unwrap().workspace = Arc::new(Workspaces {
        records: [(old_id.clone(), record.clone())].into(),
        ..Default::default()
    });
    let first = SessionId::new("row-001").unwrap();
    let last = SessionId::new("row-300").unwrap();
    seed_members(
        &old,
        &old_domain,
        &old_id,
        vec![last.clone(), first.clone()],
    )
    .await;
    old.insert_workspace_session_before(
        CallOrigin::Local,
        old_id.clone(),
        first.clone(),
        Some(last.clone()),
    )
    .unwrap()
    .await
    .unwrap();
    let saved = serde_json::to_value(old.document().as_ref()).unwrap();
    old.close().await;

    let mut current = owner().await;
    record.id = WorkspaceId::parse("f".repeat(64)).unwrap();
    let current_id = record.id.clone();
    let current_domain = TestDomain::new(current.domain.spec().clone()).await;
    let editable = Arc::get_mut(&mut current).unwrap();
    editable.domain = current_domain.domain.clone();
    editable.workspace = Arc::new(Workspaces {
        records: [(current_id.clone(), record)].into(),
        ..Default::default()
    });
    let mut restored: Document = serde_json::from_value(saved).unwrap();
    restored.validate().unwrap();
    current_domain.seed_document(&restored).await;
    editable.state.lock().unwrap().document = Arc::new(restored);
    assert!(account(&current, &current_id).await.is_empty());
    let summary = current.store.session_summary(&first).await.unwrap();
    assert!(
        current
            .workspace_catalog_metadata(&[summary])
            .await
            .unwrap()[0]
            .account
            .is_none()
    );
    assert_eq!(
        current
            .insert_workspace_session_before(CallOrigin::Local, old_id.clone(), first.clone(), None)
            .unwrap()
            .await
            .unwrap(),
        WorkspaceMoveOutcome::UnknownWorkspace
    );
    for member in [&first, &last] {
        current
            .attach_workspace_session(CallOrigin::Local, current_id.clone(), member.clone())
            .unwrap()
            .await
            .unwrap();
        if member == &first {
            assert_eq!(
                current.document().workspace_orders[&old_id].members,
                std::slice::from_ref(&last)
            );
        }
    }
    current
        .insert_workspace_session_before(
            CallOrigin::Local,
            current_id.clone(),
            first.clone(),
            Some(last),
        )
        .unwrap()
        .await
        .unwrap();
    assert_eq!(account(&current, &current_id).await[0], first);
    assert_eq!(current.document().revision, 4);
    assert!(!current.document().workspace_orders.contains_key(&old_id));
    assert_eq!(current.document().workspace_orders.len(), 1);
    let mut durable: Document =
        serde_json::from_value(serde_json::to_value(current.document().as_ref()).unwrap()).unwrap();
    durable.validate().unwrap();
    assert_eq!(durable.workspace_orders[&current_id].members.len(), 2);
    current.close().await;
}

#[tokio::test]
async fn registration_lookup_failure_stops_unpolled_account_groups() {
    let mut current = owner().await;
    for number in 400..420 {
        append_member(&current, number, &format!("/group-{number}")).await;
    }
    let workspaces = Arc::new(Workspaces {
        fail: true,
        ..Default::default()
    });
    Arc::get_mut(&mut current).unwrap().workspace = workspaces.clone();
    assert!(matches!(
        current
            .order_seed(&CallOrigin::Local, OrderScope::All)
            .unwrap()
            .await,
        Err(ApiError::Unavailable)
    ));
    let calls = workspaces.calls.load(std::sync::atomic::Ordering::SeqCst);
    assert!(
        (1..=4).contains(&calls),
        "failed lookup must not drain the other coordinate groups: {calls}"
    );
    current.close().await;
}
