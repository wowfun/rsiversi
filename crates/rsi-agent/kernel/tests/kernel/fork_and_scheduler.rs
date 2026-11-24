use super::*;
use rsi_agent_store_protocol::MAXIMUM_STORE_FACT_PAGE_BYTES as FACT_PAGE_BYTES;

#[tokio::test]
async fn working_directory_callback_requires_the_owning_live_tool() {
    let temp = tempfile::tempdir().unwrap();
    let base = rsi_workspace_path::canonical_native_path(temp.path().canonicalize().unwrap());
    let selected = base.join("target");
    std::fs::create_dir(&selected).unwrap();
    let header = SessionHeader::new_local(
        SessionId::new("directory-tool").unwrap(),
        1,
        base.to_str().unwrap(),
        AgentPresetId::new("test-agent").unwrap(),
        profile(),
    )
    .unwrap();
    let store = Arc::new(MemoryStore::new());
    let kernel = kernel(store.clone()).await;
    let worker = kernel.start_workers();
    kernel
        .submit_message(SubmitMessage {
            session: fresh(header.clone()),
            message: mailbox_message("start-directory-tool"),
            delivery: rsi_agent_session_protocol::MessageDelivery::NextTurn,
        })
        .await
        .unwrap();
    let _executor = kernel.register("directory-tool-worker".into()).unwrap();
    let mut claim = kernel
        .claim("directory-tool-worker", CancellationToken::new())
        .await
        .unwrap()
        .unwrap();
    let caller = control_tool_caller(&kernel, &claim).await;
    let cwd = kernel
        .tool_working_directory(&caller, Some("target"))
        .await
        .unwrap();
    assert_eq!(cwd.path(), selected.to_str().unwrap());
    kernel.refresh_working_directory(&mut claim).await.unwrap();
    assert_eq!(claim.working_coordinates(), &cwd);
    kernel
        .finish_turn(&claim, &TurnOutcome::Completed)
        .await
        .unwrap();
    assert!(matches!(
        kernel.tool_working_directory(&caller, Some("..")).await,
        Err(rsi_agent_turn_protocol::TurnError::StaleClaim)
    ));
    assert_eq!(
        store
            .session_summary(header.session_id())
            .await
            .unwrap()
            .working_coordinates(),
        &cwd
    );
    kernel.shutdown(worker).await.unwrap();
}

#[tokio::test]
async fn working_directory_moves_a_resident_root_and_recovers_only_that_session() {
    use rsi_agent_session_protocol::MessageDelivery;
    use rsi_agent_store_sqlite::SqliteStore;

    let temp = tempfile::tempdir().unwrap();
    let root = rsi_workspace_path::canonical_native_path(temp.path().canonicalize().unwrap());
    let [a, b, c] = ["A", "B", "C"].map(|name| root.join(name));
    for dir in [&a, &b, &c] {
        std::fs::create_dir(dir).unwrap();
    }
    let [chat, other] = ["chat", "other"].map(|id| {
        SessionHeader::new_local(
            SessionId::new(id).unwrap(),
            1,
            a.to_str().unwrap(),
            AgentPresetId::new("test-agent").unwrap(),
            profile(),
        )
        .unwrap()
    });
    let open = || Arc::new(SqliteStore::open(&root.join("store")).unwrap());
    let store = open();
    store.save_blank(other.clone(), None).await.unwrap();
    let kernel =
        AgentKernel::recover_with_clock(store.clone(), composition(), Arc::new(FixedClock))
            .await
            .unwrap();
    let worker = kernel.start_workers();
    kernel
        .submit_message(SubmitMessage {
            session: fresh(chat.clone()),
            message: mailbox_message("first-turn"),
            delivery: MessageDelivery::NextTurn,
        })
        .await
        .unwrap();
    let executor = kernel.register("root-worker".into()).unwrap();
    let mut claim = kernel
        .claim("root-worker", CancellationToken::new())
        .await
        .unwrap()
        .unwrap();
    kernel.refresh_working_directory(&mut claim).await.unwrap();
    assert_eq!(claim.working_coordinates(), chat.coordinates());

    let moved = kernel
        .working_directory(&chat, Some(b.to_str().unwrap()))
        .await
        .unwrap();
    assert_eq!(moved.path(), b.to_str().unwrap());
    kernel.refresh_working_directory(&mut claim).await.unwrap();
    assert_eq!(claim.working_coordinates(), &moved);
    let first = store.session_summary(chat.session_id()).await.unwrap();
    assert_eq!(
        kernel.working_directory(&chat, Some(".")).await.unwrap(),
        moved
    );
    assert_eq!(
        store.session_summary(chat.session_id()).await.unwrap(),
        first
    );
    let next = kernel.working_directory(&chat, Some("../C")).await.unwrap();
    assert_eq!(next.path(), c.to_str().unwrap());
    assert_eq!(
        kernel.working_directory(&other, None).await.unwrap(),
        *other.coordinates()
    );
    kernel.refresh_working_directory(&mut claim).await.unwrap();
    assert_eq!(claim.working_coordinates(), &next);
    let pending = store
        .inspect_session(chat.session_id())
        .await
        .unwrap()
        .pending;
    assert_eq!(pending.len(), 2);
    for (notice, (from, to)) in pending.iter().zip([(&a, &b), (&b, &c)]) {
        let message = store
            .read_agent_message(chat.session_id(), &notice.message_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(message.delivery, MessageDelivery::NextStep);
        assert_eq!(
            message.message.content,
            vec![AgentMessageContent::Text {
                text: format!(
                    "The working directory changed from {} to {}.",
                    serde_json::json!(from),
                    serde_json::json!(to)
                ),
            }]
        );
    }
    kernel.claim_pending_step_messages(&claim).await.unwrap();
    assert_spawn_uses_working_directory(&kernel, &claim, &*store).await;
    kernel
        .finish_turn(&claim, &TurnOutcome::Completed)
        .await
        .unwrap();
    drop(executor);
    kernel.shutdown(worker).await.unwrap();
    drop(kernel);
    drop(store);

    let store = open();
    let kernel =
        AgentKernel::recover_with_clock(store.clone(), composition(), Arc::new(FixedClock))
            .await
            .unwrap();
    assert_eq!(kernel.working_directory(&chat, None).await.unwrap(), next);
    assert_eq!(
        store
            .session_summary(chat.session_id())
            .await
            .unwrap()
            .header,
        chat
    );
    std::fs::rename(&c, root.join("moved-C")).unwrap();
    assert_eq!(
        kernel.working_directory(&chat, None).await.unwrap(),
        *chat.coordinates()
    );
    std::fs::create_dir(&c).unwrap();
    assert_eq!(
        kernel.working_directory(&chat, None).await.unwrap(),
        *chat.coordinates()
    );
    let inbox = store.inspect_session(chat.session_id()).await.unwrap();
    assert!(!inbox.tree.session.has_waking_message);
    assert_eq!(inbox.pending.len(), 1);
    assert!(
        store
            .session_summary(other.session_id())
            .await
            .unwrap()
            .is_blank()
    );
}

#[tokio::test]
async fn redirected_child_restores_once_and_holds_its_notice_over_restart() {
    use rsi_agent_session_protocol::{ExecutionCoordinates, ExecutionLocation, MessageDelivery};
    use rsi_agent_store_sqlite::SqliteStore;
    let temp = tempfile::tempdir().unwrap();
    let root = rsi_workspace_path::canonical_native_path(temp.path().canonicalize().unwrap());
    let origin = root.join("A");
    let selected = root.join("B");
    std::fs::create_dir(&origin).unwrap();
    std::fs::create_dir(&selected).unwrap();
    let open = || Arc::new(SqliteStore::open(&root.join("store")).unwrap());
    let store = open();
    let parent = SessionHeader::new_local(
        SessionId::new("parent").unwrap(),
        1,
        origin.to_str().unwrap(),
        AgentPresetId::new("test-agent").unwrap(),
        profile(),
    )
    .unwrap();
    store.save_blank(parent.clone(), None).await.unwrap();
    let kernel =
        AgentKernel::recover_with_clock(store.clone(), composition(), Arc::new(FixedClock))
            .await
            .unwrap();
    let child = kernel
        .create_plugin_child(rsi_agent_turn_protocol::CreatePluginChild {
            source: rsi_agent_turn_protocol::PluginChildSource::Session(Box::new(parent)),
            directory: Some(
                ExecutionCoordinates::new(ExecutionLocation::Local, selected.to_str().unwrap())
                    .unwrap(),
            ),
            session_id: SessionId::new("child").unwrap(),
            task_name: "check".into(),
            plugin: "example".into(),
            preset: None,
        })
        .await
        .unwrap();
    assert_eq!(
        kernel.working_directory(&child, None).await.unwrap(),
        *child.coordinates()
    );
    std::fs::rename(&selected, root.join("moved-B")).unwrap();
    std::fs::rename(&origin, root.join("moved-A")).unwrap();
    assert!(kernel.working_directory(&child, None).await.is_err());
    assert!(
        store
            .session_summary(child.session_id())
            .await
            .unwrap()
            .is_blank()
    );
    std::fs::rename(root.join("moved-A"), &origin).unwrap();
    let (a, b) = tokio::join!(
        kernel.working_directory(&child, None),
        kernel.working_directory(&child, None)
    );
    assert_eq!(a.unwrap(), *child.creation_coordinates());
    assert_eq!(b.unwrap(), *child.creation_coordinates());
    let inbox = store.inspect_session(child.session_id()).await.unwrap();
    assert!(!inbox.tree.session.has_waking_message);
    assert_eq!(inbox.pending.len(), 1);
    let notice = store
        .read_agent_message(child.session_id(), &inbox.pending[0].message_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(notice.delivery, MessageDelivery::NextStep);
    assert!(
        matches!(&notice.message.source, AgentMessageSource::Application { attribution: Some(a) } if a.source == serde_json::json!({"kind":"working-directory"}))
    );
    assert_eq!(
        notice.message.content,
        vec![rsi_agent_session_protocol::AgentMessageContent::Text {
            text: format!(
                "The working directory {} is unavailable. The working directory is now {}.",
                serde_json::json!(selected),
                serde_json::json!(origin)
            ),
        }]
    );
    let before = store.session_summary(child.session_id()).await.unwrap();
    assert!(before.working_directory.is_some());
    drop(kernel);
    drop(store);
    std::fs::create_dir(&selected).unwrap();
    let reopened = open();
    let kernel =
        AgentKernel::recover_with_clock(reopened.clone(), composition(), Arc::new(FixedClock))
            .await
            .unwrap();
    assert_eq!(
        kernel.working_directory(&child, None).await.unwrap(),
        *child.creation_coordinates()
    );
    assert_eq!(
        reopened.session_summary(child.session_id()).await.unwrap(),
        before
    );
    let worker = kernel.start_workers();
    kernel
        .submit_message(SubmitMessage {
            session: SubmitSession::Resume(
                kernel.prepare_resume(child.session_id()).await.unwrap(),
            ),
            message: mailbox_message("wake"),
            delivery: MessageDelivery::NextTurn,
        })
        .await
        .unwrap();
    let _lease = kernel.register("directory-worker".into()).unwrap();
    let mut claim = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        kernel.claim("directory-worker", CancellationToken::new()),
    )
    .await
    .unwrap()
    .unwrap()
    .unwrap();
    kernel.refresh_working_directory(&mut claim).await.unwrap();
    assert_eq!(claim.working_coordinates().path(), origin.to_str().unwrap());
    std::fs::rename(&origin, root.join("moved-A")).unwrap();
    assert!(kernel.refresh_working_directory(&mut claim).await.is_err());
    std::fs::rename(root.join("moved-A"), &origin).unwrap();
    kernel.refresh_working_directory(&mut claim).await.unwrap();
    kernel.claim_pending_step_messages(&claim).await.unwrap();
    assert!(
        reopened
            .inspect_session(child.session_id())
            .await
            .unwrap()
            .pending
            .is_empty()
    );
    assert_spawn_uses_working_directory(&kernel, &claim, &*reopened).await;
    kernel
        .finish_turn(&claim, &TurnOutcome::Completed)
        .await
        .unwrap();
    kernel.shutdown(worker).await.unwrap();
}

async fn assert_spawn_uses_working_directory(
    kernel: &AgentKernel,
    claim: &rsi_agent_turn_protocol::TurnClaim,
    store: &dyn SessionStore,
) {
    let caller = control_tool_caller(kernel, claim).await;
    let id = SessionId::new("grandchild").unwrap();
    for _ in 0..2 {
        kernel
            .spawn_agent(SpawnAgentRequest {
                caller: caller.clone(),
                child_session_id: id.clone(),
                task_name: "delegated".into(),
                message_id: MessageId::new("grandchild-input").unwrap(),
                message: "inspect the current workspace".into(),
                fork_turns: ForkTurnSelection::None,
                output_contract: None,
                model: None,
                role: None,
                reasoning_effort: None,
                cancellation: CancellationToken::new(),
            })
            .await
            .unwrap();
    }
    let actual = store.session_summary(&id).await.unwrap().header;
    assert_eq!(actual.coordinates(), claim.working_coordinates());
    assert_eq!(
        actual.creation_coordinates(),
        claim.header().creation_coordinates()
    );
}

#[tokio::test]
async fn plugin_child_runs_and_settles_with_an_empty_idle_parent() {
    let store = Arc::new(MemoryStore::new());
    let parent = header("plugin-parent");
    store.save_blank(parent.clone(), None).await.unwrap();
    let kernel = kernel(store.clone()).await;
    let worker = kernel.start_workers();
    let child = kernel
        .create_plugin_child(rsi_agent_turn_protocol::CreatePluginChild {
            source: rsi_agent_turn_protocol::PluginChildSource::Session(Box::new(parent.clone())),
            directory: Some(
                rsi_agent_session_protocol::ExecutionCoordinates::new(
                    rsi_agent_session_protocol::ExecutionLocation::Local,
                    "/sibling",
                )
                .unwrap(),
            ),
            session_id: SessionId::new("plugin-child").unwrap(),
            task_name: "analyze".into(),
            plugin: "@example/reviewer".into(),
            preset: None,
        })
        .await
        .unwrap();
    let mut message = mailbox_message("plugin-child-request");
    message.source = AgentMessageSource::Application { attribution: None };
    kernel
        .submit_message(SubmitMessage {
            session: fresh(child.clone()),
            message,
            delivery: rsi_agent_session_protocol::MessageDelivery::NextTurn,
        })
        .await
        .unwrap();
    let _lease = kernel.register("plugin-child-worker".into()).unwrap();
    let claim = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        kernel.claim("plugin-child-worker", CancellationToken::new()),
    )
    .await
    .unwrap()
    .unwrap()
    .unwrap();
    assert_eq!(claim.session_id(), child.session_id());
    assert_eq!(claim.header().canonical_cwd(), "/sibling");
    assert_eq!(
        claim.header().settings().sandbox(),
        parent.settings().sandbox()
    );
    assert_eq!(
        store
            .completion_reservation_count(parent.session_id())
            .await
            .unwrap(),
        0
    );
    kernel
        .finish_turn(&claim, &TurnOutcome::Completed)
        .await
        .unwrap();
    assert!(
        store
            .active_activation(child.session_id())
            .await
            .unwrap()
            .is_none()
    );
    let parent = store.session_summary(parent.session_id()).await.unwrap();
    assert!(
        parent.is_blank(),
        "plugin delivery must not create an automatic parent Turn or message"
    );
    kernel.shutdown(worker).await.unwrap();
}

#[tokio::test]
async fn plugin_tool_child_records_the_exact_started_effect_and_turn() {
    use rsi_agent_session_protocol::{ExecutionOwner, MessageDelivery, PluginToolOrigin};
    use rsi_agent_turn_protocol::{CreatePluginChild, PluginChildSource};
    let store = Arc::new(MemoryStore::new());
    let kernel = kernel(store).await;
    let worker = kernel.start_workers();
    kernel
        .submit_message(SubmitMessage {
            session: fresh(header("plugin-tool-parent")),
            message: mailbox_message("trigger-plugin"),
            delivery: MessageDelivery::NextTurn,
        })
        .await
        .unwrap();
    let _lease = kernel.register("plugin-tool-worker".into()).unwrap();
    let claim = kernel
        .claim("plugin-tool-worker", CancellationToken::new())
        .await
        .unwrap()
        .unwrap();
    let caller = Arc::new(control_tool_caller(&kernel, &claim).await);
    let expected = PluginToolOrigin {
        turn_id: claim.turn_id().clone(),
        effect_id: caller.tool_effect_id().unwrap().clone(),
    };
    let make = |id: &str| CreatePluginChild {
        source: PluginChildSource::Tool(caller.clone()),
        directory: None,
        session_id: SessionId::new(id).unwrap(),
        task_name: id.into(),
        plugin: "@example/delegator".into(),
        preset: None,
    };
    let child = kernel.create_plugin_child(make("worker")).await.unwrap();
    assert!(
        matches!(child.execution_owner(), Some(ExecutionOwner::Plugin { session_id, plugin, tool: Some(origin) })
        if session_id == claim.session_id() && plugin == "@example/delegator" && origin == &expected)
    );
    kernel.release(&claim).unwrap();
    assert!(matches!(
        kernel.create_plugin_child(make("late")).await,
        Err(TurnError::StaleClaim)
    ));
    kernel.shutdown(worker).await.unwrap();
}

#[tokio::test]
#[allow(clippy::too_many_lines)] // One captured boundary is reused across pages and nested Full/Last descendants.
async fn fork_reader_authenticates_once_and_reuses_its_immutable_boundary() {
    let store = Arc::new(MemoryStore::new());
    append_terminal_history(&store, "session-fork-page-root", 300).await;
    let faults = Arc::new(FactReadRaceStore::new(store.clone()));
    let kernel =
        AgentKernel::recover_with_clock(faults.clone(), composition(), Arc::new(FixedClock))
            .await
            .unwrap();
    let worker = kernel.start_workers();
    let root_id = SessionId::new("session-fork-page-root").unwrap();
    kernel
        .submit_message(SubmitMessage {
            session: resume(&kernel, root_id.clone()).await,
            message: mailbox_message("message-fork-page-root"),
            delivery: rsi_agent_session_protocol::MessageDelivery::NextTurn,
        })
        .await
        .unwrap();
    let root_lease = kernel.register("executor-fork-page-root".into()).unwrap();
    let root_claim = kernel
        .claim("executor-fork-page-root", CancellationToken::new())
        .await
        .unwrap()
        .unwrap();
    let child_id = SessionId::new("session-fork-page-child").unwrap();
    kernel
        .spawn_agent(SpawnAgentRequest {
            output_contract: None,
            role: None,
            model: None,
            reasoning_effort: None,
            cancellation: CancellationToken::new(),
            caller: control_tool_caller(&kernel, &root_claim).await,
            child_session_id: child_id.clone(),
            task_name: "paged-child".into(),
            message_id: MessageId::new("message-fork-page-child").unwrap(),
            message: "inherit the complete balanced prefix".into(),
            fork_turns: ForkTurnSelection::All,
        })
        .await
        .unwrap();
    assert_eq!(store.fork_boundary_resolution_count(), 1);

    let child_lease = kernel.register("executor-fork-page-child".into()).unwrap();
    let child_claim = kernel
        .claim("executor-fork-page-child", CancellationToken::new())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(child_claim.session_id(), &child_id);
    let reader = kernel.open_fork_history(&child_claim).await.unwrap();
    let (_, _, header_reads) = store.activity_read_counts();
    let first = reader
        .read_facts(child_claim.session_id(), 0, MAXIMUM_FACTS_PER_READ)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(first.facts.len(), MAXIMUM_FACTS_PER_READ);
    assert_eq!(store.fork_boundary_resolution_count(), 2);
    *faults.watermark_read_fault.lock().unwrap() =
        Some((root_id.clone(), StoreError::NotFound(root_id.to_string())));
    assert!(
        matches!(
            reader
                .read_facts(child_claim.session_id(), first.through_parent_seq, 1)
                .await,
            Err(TurnError::SessionNotFound(_))
        ),
        "a retained proof cannot bypass current parent availability"
    );
    let second = reader
        .read_facts(
            child_claim.session_id(),
            first.through_parent_seq,
            MAXIMUM_FACTS_PER_READ,
        )
        .await
        .unwrap()
        .unwrap();
    let (_, _, subsequent_header_reads) = store.activity_read_counts();
    *faults.watermark_read_fault.lock().unwrap() =
        Some((root_id.clone(), StoreError::NotFound(root_id.to_string())));
    assert!(
        matches!(
            reader
                .read_facts(child_claim.session_id(), second.through_parent_seq, 1)
                .await,
            Err(TurnError::SessionNotFound(_))
        ),
        "the final empty page must still observe parent retirement"
    );
    assert_eq!(
        subsequent_header_reads, header_reads,
        "pages must not re-read ancestry Headers"
    );
    assert_eq!(
        second.through_parent_seq,
        child_claim
            .header()
            .history_origin()
            .unwrap()
            .resolved_terminal_seq
    );
    assert_eq!(second.facts.len(), 88);
    assert_eq!(
        store.fork_boundary_resolution_count(),
        2,
        "later pages must reuse the boundary established at the initial cursor"
    );

    // A nested Full retains the parent's inherited selection even before the
    // parent has completed a local Turn. A Last window contains only local Turns.
    for (name, selection, expected) in [
        ("grand-full", ForkTurnSelection::All, 2),
        ("grand-last", ForkTurnSelection::Last(1), 1),
    ] {
        kernel
            .spawn_agent(SpawnAgentRequest {
                output_contract: None,
                role: None,
                model: None,
                reasoning_effort: None,
                cancellation: CancellationToken::new(),
                caller: control_tool_caller(&kernel, &child_claim).await,
                child_session_id: SessionId::new(name).unwrap(),
                task_name: name.into(),
                message_id: MessageId::new(format!("message-{name}")).unwrap(),
                message: "nested source cut".into(),
                fork_turns: selection,
            })
            .await
            .unwrap();
        let lease = kernel.register(name.into()).unwrap();
        let claim = kernel
            .claim(name, CancellationToken::new())
            .await
            .unwrap()
            .unwrap();
        let reader = kernel.open_fork_history(&claim).await.unwrap();
        let sources = reader.sources();
        assert_eq!(sources.len(), expected);
        assert_eq!(sources.last().unwrap(), claim.header());
        if expected == 2 {
            assert_eq!(sources[0], *child_claim.header());
            let inherited = reader
                .read_facts(sources[0].session_id(), 0, MAXIMUM_FACTS_PER_READ)
                .await
                .unwrap()
                .unwrap();
            assert_eq!(inherited.facts[0], first.facts[0]);
            let last = reader
                .read_facts(
                    sources[0].session_id(),
                    inherited.through_parent_seq,
                    MAXIMUM_FACTS_PER_READ,
                )
                .await
                .unwrap()
                .unwrap();
            assert_eq!(last.through_parent_seq, second.through_parent_seq);
            *faults.watermark_read_fault.lock().unwrap() =
                Some((root_id.clone(), StoreError::NotFound(root_id.to_string())));
            assert!(
                matches!(
                    reader.read_facts(sources[1].session_id(), 0, 1).await,
                    Err(TurnError::SessionNotFound(_))
                ),
                "a consumed ancestor still belongs to the authenticated chain"
            );
        }
        assert!(reader.read_facts(&root_id, 0, 1).await.is_err());
        kernel
            .finish_turn(&claim, &TurnOutcome::Completed)
            .await
            .unwrap();
        assert!(
            reader
                .read_facts(sources[0].session_id(), 0, 1)
                .await
                .is_err(),
            "retained history cannot outlive its claim"
        );
        drop(lease);
    }

    kernel
        .finish_turn(&child_claim, &TurnOutcome::Completed)
        .await
        .unwrap();
    let checkpoint = kernel
        .open_checkpoint_fork_history(&child_claim)
        .await
        .unwrap()
        .unwrap();
    faults.pause_next_read();
    let checkpoint_page = tokio::spawn({
        let reader = checkpoint.clone();
        let child_id = child_id.clone();
        let after = first.through_parent_seq;
        async move {
            reader
                .read_facts(&child_id, after, MAXIMUM_FACTS_PER_READ)
                .await
        }
    });
    tokio::time::timeout(
        std::time::Duration::from_secs(2),
        faults.read_captured.notified(),
    )
    .await
    .expect("checkpoint did not reach its final inherited page");
    *faults.boundary_read_refusal.lock().unwrap() =
        Some((child_id.clone(), StoreError::NotFound(child_id.to_string())));
    faults.release_read.notify_one();
    assert!(
        matches!(
            checkpoint_page.await.unwrap(),
            Err(TurnError::SessionNotFound(_))
        ),
        "the last page must observe child retirement during its Store read"
    );
    *faults.boundary_read_refusal.lock().unwrap() = None;
    assert_eq!(
        checkpoint
            .read_facts(&child_id, first.through_parent_seq, MAXIMUM_FACTS_PER_READ)
            .await
            .unwrap()
            .unwrap()
            .facts,
        second.facts
    );
    kernel
        .finish_turn(&root_claim, &TurnOutcome::Completed)
        .await
        .unwrap();
    drop((child_lease, root_lease));
    kernel.shutdown(worker).await.unwrap();
}

#[tokio::test]
async fn a_busy_session_message_does_not_block_an_idle_child_in_the_same_tree() {
    let store = Arc::new(MemoryStore::new());
    let kernel = kernel(store).await;
    let worker = kernel.start_workers();
    let root_id = SessionId::new("session-a-busy-root").unwrap();
    kernel
        .submit_message(SubmitMessage {
            session: fresh(header(root_id.as_str())),
            message: mailbox_message("message-root-active"),
            delivery: rsi_agent_session_protocol::MessageDelivery::NextTurn,
        })
        .await
        .unwrap();
    let _root_lease = kernel.register("executor-busy-root".into()).unwrap();
    let root_claim = kernel
        .claim("executor-busy-root", CancellationToken::new())
        .await
        .unwrap()
        .unwrap();
    let caller = control_tool_caller(&kernel, &root_claim).await;

    kernel
        .submit_message(SubmitMessage {
            session: resume(&kernel, root_id.clone()).await,
            message: mailbox_message("message-root-queued"),
            delivery: rsi_agent_session_protocol::MessageDelivery::NextTurn,
        })
        .await
        .unwrap();
    let child_id = SessionId::new("session-z-idle-child").unwrap();
    kernel
        .spawn_agent(SpawnAgentRequest {
            output_contract: None,
            role: None,
            model: None,
            reasoning_effort: None,
            cancellation: CancellationToken::new(),
            caller,
            child_session_id: child_id.clone(),
            task_name: "idle-child".into(),
            message_id: MessageId::new("message-child-ready").unwrap(),
            message: "claim me while the root remains busy".into(),
            fork_turns: ForkTurnSelection::None,
        })
        .await
        .unwrap();

    let _child_lease = kernel.register("executor-idle-child".into()).unwrap();
    let child_claim = tokio::time::timeout(
        std::time::Duration::from_secs(1),
        kernel.claim("executor-idle-child", CancellationToken::new()),
    )
    .await
    .expect("the busy root's earlier ready message must not block its idle child")
    .unwrap()
    .unwrap();
    assert_eq!(child_claim.session_id(), &child_id);

    kernel
        .finish_turn(&child_claim, &TurnOutcome::Completed)
        .await
        .unwrap();
    kernel
        .finish_turn(&root_claim, &TurnOutcome::Completed)
        .await
        .unwrap();
    kernel.shutdown(worker).await.unwrap();
}

#[tokio::test]
async fn concurrent_executor_lanes_contend_for_one_ready_message_without_failure() {
    let store = Arc::new(MemoryStore::new());
    let kernel = kernel(store).await;
    let worker = kernel.start_workers();
    let session_id = SessionId::new("session-concurrent-ready-claim").unwrap();
    kernel
        .submit_message(SubmitMessage {
            session: fresh(header(session_id.as_str())),
            message: mailbox_message("message-concurrent-ready-claim"),
            delivery: rsi_agent_session_protocol::MessageDelivery::NextTurn,
        })
        .await
        .unwrap();
    let _one = kernel.register("executor-ready-one".into()).unwrap();
    let _two = kernel.register("executor-ready-two".into()).unwrap();
    let cancellation = CancellationToken::new();
    let barrier = Arc::new(tokio::sync::Barrier::new(3));
    let mut tasks = tokio::task::JoinSet::new();
    for executor in ["executor-ready-one", "executor-ready-two"] {
        let kernel = kernel.clone();
        let cancellation = cancellation.child_token();
        let barrier = Arc::clone(&barrier);
        tasks.spawn(async move {
            barrier.wait().await;
            kernel.claim(executor, cancellation).await
        });
    }
    barrier.wait().await;
    let first = tokio::time::timeout(std::time::Duration::from_secs(1), tasks.join_next())
        .await
        .expect("one claim lane did not acquire the ready message")
        .expect("claim task set ended early")
        .unwrap()
        .expect("ready-message contention escaped as a claim failure")
        .expect("one claim lane must acquire the ready message");
    cancellation.cancel();
    assert!(
        tokio::time::timeout(std::time::Duration::from_secs(1), tasks.join_next())
            .await
            .expect("the losing claim lane did not observe cancellation")
            .expect("claim task set ended early")
            .unwrap()
            .expect("the losing claim lane failed during ordinary contention")
            .is_none()
    );

    kernel
        .finish_turn(&first, &TurnOutcome::Completed)
        .await
        .unwrap();
    kernel.shutdown(worker).await.unwrap();
}

#[tokio::test]
#[allow(clippy::too_many_lines)] // Cancellation and output-limit cases share the full live-descendant and preserved-inbox contract.
async fn cancelled_or_max_tokens_parent_cascades_without_erasing_child_inbox() {
    for (label, parent_outcome) in [
        ("cancel", None),
        ("max_tokens", Some(TurnOutcome::MaxTokens)),
    ] {
        let store = Arc::new(MemoryStore::new());
        let kernel = kernel(store.clone()).await;
        let worker = kernel.start_workers();
        let root_id = SessionId::new(format!("session-cascade-root-{label}")).unwrap();
        let root_message = MessageId::new(format!("message-cascade-root-{label}")).unwrap();
        kernel
            .submit_message(SubmitMessage {
                session: fresh(header(root_id.as_str())),
                message: mailbox_message(root_message.as_str()),
                delivery: rsi_agent_session_protocol::MessageDelivery::NextTurn,
            })
            .await
            .unwrap();
        kernel
            .claim_message(ClaimMessage {
                session: kernel.prepare_resume(&root_id).await.unwrap(),
                message_id: root_message,
                activation_id: ActivationId::new(format!("activation-cascade-root-{label}"))
                    .unwrap(),
                path: AgentPath::root(),
                turn_id: TurnId::new(format!("turn-cascade-root-{label}")).unwrap(),
                step_id: StepId::new(format!("step-cascade-root-{label}")).unwrap(),
            })
            .await
            .unwrap();
        let _root_lease = kernel
            .register(format!("executor-cascade-root-{label}"))
            .unwrap();
        let root_claim = kernel
            .claim(
                &format!("executor-cascade-root-{label}"),
                CancellationToken::new(),
            )
            .await
            .unwrap()
            .unwrap();
        let caller = control_tool_caller(&kernel, &root_claim).await;
        let child_id = SessionId::new(format!("session-cascade-child-{label}")).unwrap();
        kernel
            .spawn_agent(SpawnAgentRequest {
                output_contract: None,
                role: None,
                model: None,
                reasoning_effort: None,
                cancellation: CancellationToken::new(),
                caller: caller.clone(),
                child_session_id: child_id.clone(),
                task_name: format!("child-{label}"),
                message_id: MessageId::new(format!("message-cascade-child-{label}")).unwrap(),
                message: "perform child work".into(),
                fork_turns: ForkTurnSelection::None,
            })
            .await
            .unwrap();
        let _child_lease = kernel
            .register(format!("executor-cascade-child-{label}"))
            .unwrap();
        let child_claim = kernel
            .claim(
                &format!("executor-cascade-child-{label}"),
                CancellationToken::new(),
            )
            .await
            .unwrap()
            .unwrap();
        let child_cancellation = kernel.cancellation(&child_claim).unwrap();
        let queued = kernel
            .submit(SubmitTurn {
                reasoning_effort: None,
                session: resume(&kernel, child_id.clone()).await,
                turn_id: TurnId::new(format!("turn-queued-child-{label}")).unwrap(),
                text: "already accepted behind the running child".into(),
                model: None,
                sandbox: None,
            })
            .await
            .unwrap();
        kernel
            .flush(&child_claim, queued.accepted_seq)
            .await
            .unwrap();
        let held_message = MessageId::new(format!("message-held-child-{label}")).unwrap();
        kernel
            .send_agent_message(SendAgentMessage {
                cancellation: CancellationToken::new(),
                caller,
                target_session_id: child_id.clone(),
                message_id: held_message.clone(),
                message: "retain this accepted follow-up".into(),
                start_new_turn: false,
            })
            .await
            .unwrap();

        let outcome = if let Some(outcome) = parent_outcome {
            kernel
                .close_current_step(&root_claim, &outcome)
                .await
                .unwrap();
            outcome
        } else {
            kernel
                .cancel(&root_id, root_claim.turn_id(), Some("stop tree".into()))
                .await
                .unwrap();
            TurnOutcome::Cancelled
        };
        kernel.finish_turn(&root_claim, &outcome).await.unwrap();
        assert!(
            child_cancellation.is_cancelled(),
            "{label} must durably cascade to a live child"
        );
        assert_eq!(
            store
                .active_activation(&root_id)
                .await
                .unwrap()
                .unwrap()
                .phase,
            StoreActivationPhase::WaitingForDescendants
        );
        assert!(matches!(
            kernel
                .message_status(&child_id, &held_message)
                .await
                .unwrap()
                .state,
            MessageState::Pending
        ));

        kernel
            .finish_turn(&child_claim, &TurnOutcome::Cancelled)
            .await
            .unwrap();
        let queued_claim = kernel
            .claim(
                &format!("executor-cascade-child-{label}"),
                CancellationToken::new(),
            )
            .await
            .unwrap()
            .unwrap();
        assert_eq!(queued_claim.turn_id(), &queued.turn_id);
        assert!(kernel.cancellation(&queued_claim).unwrap().is_cancelled());
        let terminal = kernel
            .publish(
                &queued_claim,
                vec![SessionFactBody::TurnTerminal {
                    turn_id: queued.turn_id,
                    outcome: TurnOutcome::Cancelled,
                    result: None,
                }],
            )
            .await
            .unwrap()
            .published();
        kernel
            .flush(&queued_claim, terminal[0].seq())
            .await
            .unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            while store.active_activation(&root_id).await.unwrap().is_some() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("the drained descendant must release its waiting ancestors");
        assert!(store.active_activation(&child_id).await.unwrap().is_none());
        assert!(matches!(
            kernel
                .message_status(&child_id, &held_message)
                .await
                .unwrap()
                .state,
            MessageState::Pending
        ));
        kernel.shutdown(worker).await.unwrap();
    }
}

#[tokio::test]
async fn cancelling_an_unclaimed_message_is_durable_and_idempotent() {
    let store = Arc::new(MemoryStore::new());
    let kernel = kernel(store).await;
    let worker = kernel.start_workers();
    let session_id = SessionId::new("session-message-cancel").unwrap();
    let message_id = MessageId::new("message-cancel").unwrap();
    kernel
        .submit_message(SubmitMessage {
            session: fresh(header(session_id.as_str())),
            message: mailbox_message(message_id.as_str()),
            delivery: rsi_agent_session_protocol::MessageDelivery::NextTurn,
        })
        .await
        .unwrap();
    assert!(
        kernel
            .cancel_target(&session_id, CancelTarget::Message(message_id.clone()), None)
            .await
            .unwrap()
            .accepted
    );
    let repeated = kernel
        .cancel_target(&session_id, CancelTarget::Message(message_id.clone()), None)
        .await
        .unwrap();
    assert!(!repeated.accepted);
    assert!(repeated.already_terminal);
    assert!(matches!(
        kernel
            .message_status(&session_id, &message_id)
            .await
            .unwrap()
            .state,
        MessageState::Discarded { .. }
    ));
    kernel.shutdown(worker).await.unwrap();
}

#[tokio::test]
async fn recovery_resumes_a_durably_parked_wait_before_interrupting_its_activation() {
    let store = Arc::new(MemoryStore::new());
    let initial = kernel(store.clone()).await;
    let worker = initial.start_workers();
    let root_id = SessionId::new("session-parked-recovery-root").unwrap();
    initial
        .submit_message(SubmitMessage {
            session: fresh(header(root_id.as_str())),
            message: mailbox_message("message-parked-recovery-root"),
            delivery: rsi_agent_session_protocol::MessageDelivery::NextTurn,
        })
        .await
        .unwrap();
    let _root_lease = initial
        .register("executor-parked-recovery-root".into())
        .unwrap();
    let root_claim = initial
        .claim("executor-parked-recovery-root", CancellationToken::new())
        .await
        .unwrap()
        .unwrap();
    let caller = control_tool_caller(&initial, &root_claim).await;
    let child_id = SessionId::new("session-parked-recovery-child").unwrap();
    initial
        .spawn_agent(SpawnAgentRequest {
            output_contract: None,
            role: None,
            model: None,
            reasoning_effort: None,
            cancellation: CancellationToken::new(),
            caller: caller.clone(),
            child_session_id: child_id,
            task_name: "parked-recovery-child".into(),
            message_id: MessageId::new("message-parked-recovery-child").unwrap(),
            message: "keep the parent wait parked".into(),
            fork_turns: ForkTurnSelection::None,
        })
        .await
        .unwrap();
    let waiter = tokio::spawn({
        let initial = initial.clone();
        async move {
            initial
                .wait_agent(
                    &caller,
                    std::time::Duration::from_mins(1),
                    CancellationToken::new(),
                )
                .await
        }
    });
    tokio::time::timeout(std::time::Duration::from_secs(1), async {
        loop {
            if store
                .active_activation(&root_id)
                .await
                .unwrap()
                .is_some_and(|active| active.phase == StoreActivationPhase::Parked)
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("wait did not durably park");

    initial.shutdown(worker).await.unwrap();
    assert!(waiter.await.unwrap().is_err());
    let recovered = kernel(store.clone()).await;
    assert!(matches!(
        recovered
            .outcome(&root_id, root_claim.turn_id())
            .await
            .unwrap(),
        Some(TurnOutcome::Interrupted { .. })
    ));
    let controls = store
        .read_controls(&root_id, 0, 32, FACT_PAGE_BYTES)
        .await
        .unwrap();
    let resumed = controls.records.iter().position(|record| {
        matches!(
            record.body(),
            AgentControlRecordBody::WaitResumed {
                cause: WaitResumeCause::Cancel,
                ..
            }
        )
    });
    let waiting = controls.records.iter().position(|record| {
        matches!(
            record.body(),
            AgentControlRecordBody::ActivationWaitingForDescendants { .. }
        )
    });
    assert!(
        resumed
            .zip(waiting)
            .is_some_and(|(resumed, waiting)| resumed < waiting)
    );
}

#[tokio::test]
async fn claimed_message_retry_reuses_the_stored_acceptance_boundary_after_cold_recovery() {
    let store = Arc::new(MemoryStore::new());
    let initial = kernel(store.clone()).await;
    let worker = initial.start_workers();
    let session_id = SessionId::new("session-claim-retry-boundary").unwrap();
    let message_id = MessageId::new("message-claim-retry-boundary").unwrap();
    initial
        .submit_message(SubmitMessage {
            session: fresh(header(session_id.as_str())),
            message: mailbox_message(message_id.as_str()),
            delivery: rsi_agent_session_protocol::MessageDelivery::NextTurn,
        })
        .await
        .unwrap();
    let activation_id = ActivationId::new("activation-claim-retry-boundary").unwrap();
    let turn_id = TurnId::new("turn-claim-retry-boundary").unwrap();
    let step_id = StepId::new("step-claim-retry-boundary").unwrap();
    let submitted = initial
        .claim_message(ClaimMessage {
            session: initial.prepare_resume(&session_id).await.unwrap(),
            message_id: message_id.clone(),
            activation_id: activation_id.clone(),
            path: AgentPath::root(),
            turn_id: turn_id.clone(),
            step_id: step_id.clone(),
        })
        .await
        .unwrap();
    assert_eq!(submitted.accepted_seq, 1);
    assert_eq!(
        initial
            .message_status(&session_id, &message_id)
            .await
            .unwrap()
            .state,
        MessageState::Claimed {
            activation_id: activation_id.clone(),
            turn_id: turn_id.clone(),
            step_id: step_id.clone(),
            claimed_fact_seq: 3,
        }
    );
    initial.shutdown(worker).await.unwrap();

    let recovered = kernel(store).await;
    let retried = recovered
        .claim_message(ClaimMessage {
            session: recovered.prepare_resume(&session_id).await.unwrap(),
            message_id,
            activation_id,
            path: AgentPath::root(),
            turn_id,
            step_id,
        })
        .await
        .unwrap();
    assert_eq!(retried.accepted_seq, submitted.accepted_seq);
}

#[tokio::test]
async fn activation_terminal_requeues_the_next_oldest_turn() {
    let store = Arc::new(MemoryStore::new());
    let kernel = kernel(store).await;
    let worker = kernel.start_workers();
    let session_id = SessionId::new("session-activation-handoff").unwrap();
    kernel
        .submit_message(SubmitMessage {
            session: fresh(header(session_id.as_str())),
            message: mailbox_message("message-activation-handoff"),
            delivery: rsi_agent_session_protocol::MessageDelivery::NextTurn,
        })
        .await
        .unwrap();
    let _root_lease = kernel
        .register("executor-activation-handoff".into())
        .unwrap();
    let activation_claim = kernel
        .claim("executor-activation-handoff", CancellationToken::new())
        .await
        .unwrap()
        .unwrap();
    let queued = kernel
        .submit(SubmitTurn {
            reasoning_effort: None,
            session: resume(&kernel, session_id.clone()).await,
            turn_id: TurnId::new("turn-after-activation").unwrap(),
            text: "queued behind activation".into(),
            model: None,
            sandbox: None,
        })
        .await
        .unwrap();
    let cancelled = CancellationToken::new();
    cancelled.cancel();
    assert!(
        kernel
            .claim("executor-activation-handoff", cancelled)
            .await
            .unwrap()
            .is_none()
    );

    kernel
        .finish_turn(&activation_claim, &TurnOutcome::Completed)
        .await
        .unwrap();
    let next = tokio::time::timeout(
        std::time::Duration::from_secs(1),
        kernel.claim("executor-activation-handoff", CancellationToken::new()),
    )
    .await
    .expect("activation terminal did not requeue the next accepted Turn")
    .unwrap()
    .unwrap();
    assert_eq!(next.turn_id(), &queued.turn_id);
    kernel.shutdown(worker).await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn completed_activation_sessions_release_resident_capacity() {
    let store = Arc::new(MemoryStore::new());
    let kernel = kernel(store).await;
    let worker = kernel.start_workers();
    let _lease = kernel
        .register("executor-activation-eviction".into())
        .unwrap();
    for index in 0..=MAXIMUM_ACTIVE_SESSIONS {
        let session_id = SessionId::new(format!("session-activation-eviction-{index}")).unwrap();
        kernel
            .submit_message(SubmitMessage {
                session: fresh(header(session_id.as_str())),
                message: mailbox_message(&format!("message-activation-eviction-{index}")),
                delivery: rsi_agent_session_protocol::MessageDelivery::NextTurn,
            })
            .await
            .unwrap();
        let claim = kernel
            .claim("executor-activation-eviction", CancellationToken::new())
            .await
            .unwrap()
            .unwrap();
        kernel
            .finish_turn(&claim, &TurnOutcome::Completed)
            .await
            .unwrap();
    }
    kernel.shutdown(worker).await.unwrap();
}

#[tokio::test]
async fn submit_message_rejects_new_turn_options_on_next_step_before_durable_admission() {
    let store = Arc::new(MemoryStore::new());
    let kernel = kernel(store.clone()).await;
    let session_id = SessionId::new("session-invalid-wake-target").unwrap();
    let mut message = mailbox_message("message-invalid-wake-target");
    message.options.sandbox = Some(SandboxMode::ReadOnly);
    assert!(matches!(
        kernel
            .submit_message(SubmitMessage {
                session: fresh(header(session_id.as_str())),
                message,
                delivery: rsi_agent_session_protocol::MessageDelivery::NextStep,
            })
            .await,
        Err(TurnError::Invalid(_))
    ));
    assert!(matches!(
        store.header(&session_id).await,
        Err(StoreError::NotFound(_))
    ));
}

#[tokio::test]
async fn fresh_message_cannot_publish_over_an_unflushed_fresh_turn_header() {
    let memory = Arc::new(MemoryStore::new());
    let store = Arc::new(FactReadRaceStore::new(memory));
    let service: Arc<dyn SessionStore> = store.clone();
    let kernel = AgentKernel::recover_with_clock(service, composition(), Arc::new(FixedClock))
        .await
        .unwrap();
    let worker = kernel.start_workers();
    let session_id = SessionId::new("session-fresh-message-race").unwrap();
    store.pause_next_append();
    let first = tokio::spawn({
        let kernel = kernel.clone();
        let session_id = session_id.clone();
        async move {
            kernel
                .submit(SubmitTurn {
                    reasoning_effort: None,
                    session: fresh(header(session_id.as_str())),
                    turn_id: TurnId::new("turn-fresh-message-race").unwrap(),
                    text: "first fresh submission".into(),
                    model: None,
                    sandbox: None,
                })
                .await
        }
    });
    store.wait_until_append_is_blocked().await;
    let competing = kernel
        .submit_message(SubmitMessage {
            session: fresh(header(session_id.as_str())),
            message: mailbox_message("message-fresh-message-race"),
            delivery: rsi_agent_session_protocol::MessageDelivery::NextTurn,
        })
        .await;
    store.release_blocked_append();
    let first = first.await.unwrap();

    assert!(matches!(competing, Err(TurnError::Invalid(_))));
    assert!(first.is_ok(), "winning fresh Turn was corrupted: {first:?}");
    kernel.shutdown(worker).await.unwrap();
}

#[tokio::test]
async fn controls_only_message_commit_retries_a_concurrent_fact_flush() {
    let memory = Arc::new(MemoryStore::new());
    let store = Arc::new(FactReadRaceStore::new(memory));
    let service: Arc<dyn SessionStore> = store.clone();
    let kernel = AgentKernel::recover_with_clock(service, composition(), Arc::new(FixedClock))
        .await
        .unwrap();
    let worker = kernel.start_workers();
    let submitted = submit(&kernel, "session-message-flush-race", "run").await;
    let _lease = kernel
        .register("executor-message-flush-race".into())
        .unwrap();
    let claim = kernel
        .claim("executor-message-flush-race", CancellationToken::new())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(claim.turn_id(), &submitted.turn_id);
    tool_origin::prepare_claimed_inputs(&kernel, &claim).await;

    store.pause_next_append();
    let pending = kernel
        .publish(
            &claim,
            vec![SessionFactBody::ModelIntent {
                evidence: rsi_agent_session_protocol::RequestEvidence::Unavailable {
                    reason: rsi_agent_session_protocol::EvidenceUnavailable::NotCaptured,
                },
                price_quote: None,
                purpose: rsi_agent_session_protocol::ModelPurpose::Conversation,
                turn_id: claim.turn_id().clone(),
                effect_id: EffectId::new("effect-message-flush-race").unwrap(),
                snapshot: snapshot(),
            }],
        )
        .await
        .unwrap()
        .published();
    let pending_seq = pending[0].seq();
    let resumed = resume(&kernel, submitted.session_id.clone()).await;
    store.pause_next_agent_commit_before_apply();
    let message = tokio::spawn({
        let kernel = kernel.clone();
        async move {
            kernel
                .submit_message(SubmitMessage {
                    session: resumed,
                    message: mailbox_message("message-flush-race"),
                    delivery: rsi_agent_session_protocol::MessageDelivery::NextStep,
                })
                .await
        }
    });
    store.wait_until_agent_commit_is_before_apply().await;

    let flush = tokio::spawn({
        let kernel = kernel.clone();
        let claim = claim.clone();
        async move { kernel.flush(&claim, pending_seq).await }
    });
    store.wait_until_append_is_blocked().await;
    store.release_blocked_append();
    assert_eq!(flush.await.unwrap().unwrap(), pending_seq);
    store.release_agent_commit_before_apply();

    let receipt = message
        .await
        .unwrap()
        .expect("a concurrent write-behind Fact flush is a retryable internal race");
    assert_eq!(receipt.observed_fact_seq, pending_seq);
    kernel.shutdown(worker).await.unwrap();
}

#[tokio::test]
async fn cancellation_cannot_diverge_resident_state_from_an_applied_activation_terminal() {
    let memory = Arc::new(MemoryStore::new());
    let store = Arc::new(FactReadRaceStore::new(memory));
    let service: Arc<dyn SessionStore> = store.clone();
    let kernel = AgentKernel::recover_with_clock(service, composition(), Arc::new(FixedClock))
        .await
        .unwrap();
    let worker = kernel.start_workers();
    let session_id = SessionId::new("session-terminal-cancel-race").unwrap();
    kernel
        .submit_message(SubmitMessage {
            session: fresh(header(session_id.as_str())),
            message: mailbox_message("message-terminal-cancel-race"),
            delivery: rsi_agent_session_protocol::MessageDelivery::NextTurn,
        })
        .await
        .unwrap();
    let _lease = kernel
        .register("executor-terminal-cancel-race".into())
        .unwrap();
    let claim = kernel
        .claim("executor-terminal-cancel-race", CancellationToken::new())
        .await
        .unwrap()
        .unwrap();

    store.pause_next_agent_commit_after_apply();
    let finish = tokio::spawn({
        let kernel = kernel.clone();
        let claim = claim.clone();
        async move { kernel.finish_turn(&claim, &TurnOutcome::Completed).await }
    });
    store.wait_until_agent_commit_is_applied().await;
    let cancel = tokio::spawn({
        let kernel = kernel.clone();
        let session_id = session_id.clone();
        let turn_id = claim.turn_id().clone();
        async move { kernel.cancel(&session_id, &turn_id, None).await }
    });
    for _ in 0..16 {
        tokio::task::yield_now().await;
    }
    store.release_applied_agent_commit();

    assert!(
        finish.await.unwrap().is_ok(),
        "applied terminal commit diverged from resident state"
    );
    let cancelled = cancel.await.unwrap().unwrap();
    assert!(!cancelled.accepted);
    assert!(cancelled.already_terminal);
    kernel.shutdown(worker).await.unwrap();
}
