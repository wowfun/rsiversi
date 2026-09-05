use super::lifecycle::flush_status_receiver;
use super::turn_state::apply_tool_body;
use super::*;

fn tool_intent(turn_id: &TurnId, suffix: &str, parallel_safe: bool) -> SessionFactBody {
    SessionFactBody::ToolIntent {
        turn_id: turn_id.clone(),
        effect_id: EffectId::new(format!("effect-{suffix}")).unwrap(),
        identity: rsi_tools_protocol::ToolResultIdentity::new(
            "owner",
            format!("invocation-{suffix}"),
            format!("call-{suffix}"),
            "a".repeat(64),
        )
        .unwrap(),
        name: format!("tool_{suffix}"),
        arguments: serde_json::json!({}),
        approval: None,
        parallel_safe,
    }
}

#[test]
fn overlapping_tool_intents_require_every_definition_to_be_parallel_safe() {
    let turn_id = TurnId::new("turn-parallel-tool-intents").unwrap();
    let first = tool_intent(&turn_id, "first", true);
    let second = tool_intent(&turn_id, "second", true);
    let exclusive = tool_intent(&turn_id, "exclusive", false);
    let mut turn = TurnControl::new(1, 1);

    apply_tool_body(&mut turn, &first).unwrap();
    apply_tool_body(&mut turn, &second).unwrap();
    assert_eq!(turn.effects.len(), 2);
    assert!(matches!(
        apply_tool_body(&mut turn, &exclusive),
        Err(TurnError::Invalid(message))
            if message.contains("parallel-safe definitions")
    ));

    let mut turn = TurnControl::new(1, 1);
    apply_tool_body(&mut turn, &exclusive).unwrap();
    assert!(matches!(
        apply_tool_body(&mut turn, &first),
        Err(TurnError::Invalid(message))
            if message.contains("parallel-safe definitions")
    ));
}

#[test]
fn next_step_message_claim_selects_an_ordered_byte_bounded_prefix() {
    let entry = |suffix: &str| {
        let message = AgentMessage {
            message_id: MessageId::new(format!("message-{suffix}")).unwrap(),
            source: AgentMessageSource::Human,
            content: vec![AgentMessageContent::Text {
                text: format!("payload-{suffix}"),
            }],
            options: MessageOptions::default(),
        };
        DurableMessageEntry {
            encoded_message_bytes: serde_json::to_vec(&message).unwrap().len(),
            message,
            root_session_id: SessionId::new("message-prefix-root").unwrap(),
            target: MessageTarget::NextStep,
            wake_required: false,
            accepted_control_seq: 1,
            state: MessageState::Pending,
        }
    };
    let first = entry("first");
    let first_bytes = serde_json::to_vec(&first.message).unwrap().len();
    let selected =
        bounded_step_message_prefix(vec![first, entry("second"), entry("third")], first_bytes)
            .unwrap();

    assert_eq!(selected.len(), 1);
    assert_eq!(selected[0].message.message_id.as_str(), "message-first");
}

#[test]
fn missing_flush_session_after_quiesce_is_a_shutdown_failure() {
    let state = KernelState {
        accepting: false,
        sessions: BTreeMap::new(),
        loading_sessions: BTreeMap::new(),
        fresh_reservations: BTreeSet::new(),
        executors: BTreeMap::new(),
        next_executor_registration: 0,
        finalizers: BTreeMap::new(),
        finalizer_names: BTreeSet::new(),
        next_finalizer_registration: 0,
        tree_lanes: BTreeMap::new(),
        next_claim: 0,
        claim_queue: VecDeque::new(),
        queued: BTreeSet::new(),
    };
    let error = flush_status_receiver(&state, &SessionId::new("session-after-quiesce").unwrap())
        .expect_err("quiesced sessions have been released");
    assert!(matches!(error, KernelError::Shutdown(_)));
}

#[tokio::test(start_paused = true)]
async fn submission_admission_wait_is_bounded() {
    let admission = Arc::new(SubmissionAdmission::new());
    let mut leases = Vec::with_capacity(MAXIMUM_ACTIVE_SESSIONS);
    for index in 0..MAXIMUM_ACTIVE_SESSIONS {
        leases.push(
            admission
                .acquire(&SessionId::new(format!("session-{index}")).unwrap())
                .await
                .unwrap(),
        );
    }
    let waiter = tokio::spawn({
        let admission = Arc::clone(&admission);
        async move {
            admission
                .acquire(&SessionId::new("session-over-capacity").unwrap())
                .await
        }
    });
    tokio::task::yield_now().await;
    tokio::time::advance(DURABILITY_WAIT_TIMEOUT).await;
    assert!(matches!(waiter.await.unwrap(), Err(TurnError::Capacity)));
    drop(leases);
}

#[tokio::test]
async fn closing_submission_admission_releases_same_session_waiters() {
    let admission = Arc::new(SubmissionAdmission::new());
    let session = SessionId::new("session-serialized").unwrap();
    let lease = admission.acquire(&session).await.unwrap();
    let waiter = tokio::spawn({
        let admission = Arc::clone(&admission);
        async move { admission.acquire(&session).await }
    });
    tokio::task::yield_now().await;
    admission.close();
    assert!(matches!(
        waiter.await.unwrap(),
        Err(TurnError::ShuttingDown)
    ));
    assert!(admission.slots.is_closed());
    drop(lease);
}

#[tokio::test]
async fn same_session_waiters_do_not_consume_unrelated_active_slots() {
    let admission = Arc::new(SubmissionAdmission::new());
    let session = SessionId::new("session-contended").unwrap();
    let lease = admission.acquire(&session).await.unwrap();
    let mut waiters = Vec::with_capacity(MAXIMUM_ACTIVE_SESSIONS - 1);
    for _ in 1..MAXIMUM_ACTIVE_SESSIONS {
        waiters.push(tokio::spawn({
            let admission = Arc::clone(&admission);
            let session = session.clone();
            async move { admission.acquire(&session).await }
        }));
    }
    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            let queued = admission
                .sessions
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .get(&session)
                .map_or(0, Weak::strong_count);
            if queued == MAXIMUM_ACTIVE_SESSIONS {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("same-Session waiters did not all reach keyed admission");

    let unrelated = tokio::time::timeout(
        Duration::from_millis(100),
        admission.acquire(&SessionId::new("session-unrelated").unwrap()),
    )
    .await
    .expect("same-Session waiters consumed every unrelated active slot")
    .expect("unrelated Session admission");
    drop(unrelated);

    admission.close();
    drop(lease);
    for waiter in waiters {
        assert!(matches!(
            waiter.await.unwrap(),
            Err(TurnError::ShuttingDown)
        ));
    }
}

#[test]
fn write_behind_deadline_rebases_after_a_slow_or_early_scan() {
    let origin = Instant::now();
    let scheduled = origin + WRITE_BEHIND_INTERVAL;
    let slow_completion = origin + WRITE_BEHIND_INTERVAL * 3;
    assert_eq!(
        rebase_write_behind_tick(scheduled, slow_completion),
        slow_completion + WRITE_BEHIND_INTERVAL
    );

    let early_notification = origin + WRITE_BEHIND_INTERVAL / 2;
    assert_eq!(
        rebase_write_behind_tick(scheduled, early_notification),
        early_notification + WRITE_BEHIND_INTERVAL
    );
}
