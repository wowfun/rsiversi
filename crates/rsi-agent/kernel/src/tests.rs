use super::turn_state::apply_tool_body;
use super::*;

#[derive(Debug)]
struct ClaimFixture;
#[async_trait]
impl rsi_tools_protocol::ToolRuntime for ClaimFixture {
    fn program_role(&self, _: &str) -> Option<rsi_tools_protocol::ToolProgramRole> {
        None
    }
    fn program_roles(&self) -> BTreeMap<String, rsi_tools_protocol::ToolProgramRole> {
        BTreeMap::new()
    }
    fn definition(&self, _: &str) -> Option<rsi_tools_protocol::ToolDefinition> {
        None
    }
    fn definitions(&self) -> Vec<rsi_tools_protocol::ToolDefinition> {
        vec![]
    }
    fn prepare(
        &self,
        _: &str,
        _: rsi_tools_protocol::ToolCall,
    ) -> rsi_tools_protocol::Result<Box<dyn rsi_tools_protocol::PreparedToolCall>> {
        unreachable!()
    }
    fn query(
        &self,
        _: &rsi_tools_protocol::ToolResultIdentity,
    ) -> rsi_tools_protocol::Result<rsi_tools_protocol::RetainedToolResult> {
        unreachable!()
    }
    async fn wait(
        &self,
        _: &rsi_tools_protocol::ToolResultIdentity,
        _: CancellationToken,
    ) -> rsi_tools_protocol::Result<rsi_tools_protocol::RetainedToolResult> {
        unreachable!()
    }
    fn commit(&self, _: &rsi_tools_protocol::ToolResultIdentity) -> rsi_tools_protocol::Result<()> {
        unreachable!()
    }
}
#[async_trait]
impl AgentComposition for ClaimFixture {
    async fn default_preset_id(
        &self,
    ) -> std::result::Result<rsi_agent_session_protocol::AgentPresetId, AgentCompositionError> {
        unreachable!()
    }
    async fn pin(
        &self,
        preset: &rsi_agent_session_protocol::AgentPresetId,
        _: Option<&rsi_agent_composition_protocol::AgentGenerationSeed>,
    ) -> std::result::Result<AgentCompositionPin, AgentCompositionError> {
        AgentCompositionPin::new(
            preset.clone(),
            "a".repeat(64),
            Arc::new(Self),
            Arc::new(rsi_agent_context::DefaultContextBuilder::default()),
            rsi_agent_composition_protocol::DomainCatalog::default(),
            rsi_agent_composition_protocol::ContributionCatalog::default(),
            Arc::new(()),
        )
    }
}

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "one poisoned and one healthy Session share the same claim queue"
)]
async fn invalid_claim_quarantines_only_its_session_and_releases_its_prepared_lane() {
    use rsi_agent_session_protocol::{AgentPresetId, FrozenAgentSettings};
    let kernel = AgentKernel::recover(
        Arc::new(rsi_agent_testkit::MemoryStore::new()),
        Arc::new(ClaimFixture),
    )
    .await
    .unwrap();
    let _executor = TurnExecution::register(&kernel, "binding-worker".into()).unwrap();
    let header = SessionHeader::new_local(
        SessionId::new("binding-session").unwrap(),
        1,
        "/workspace",
        AgentPresetId::new("test").unwrap(),
        FrozenAgentSettings::new(
            "test",
            "system",
            rsi_ai_protocol::ModelRef::new("test", "test").unwrap(),
            rsi_sandbox::SandboxMode::ReadOnly,
            false,
        )
        .unwrap(),
    )
    .unwrap();
    let session_id = header.session_id().clone();
    let turn_id = TurnId::new("binding-turn").unwrap();
    let pin = ClaimFixture
        .pin(header.agent_preset_id(), None)
        .await
        .unwrap();
    let mut session = SessionRuntime::new(header, pin, 1, false);
    let pool = Arc::new(Semaphore::new(1));
    let lane = Arc::new(TreeClaimLane {
        pool: pool.clone(),
        permit: Mutex::new(Some(pool.clone().try_acquire_owned().unwrap())),
    });
    let mut turn = TurnControl::new(1, 1);
    turn.prepared_lane = Some(lane.clone());
    // Inject an impossible admission mismatch at the defensive issuance boundary.
    turn.execution = Some(crate::execution_fixture::lease(
        rsi_execution::ExecutionLocation::Ssh {
            target: rsi_execution::ExecutionTargetId::parse("a".repeat(32)).unwrap(),
        },
        Arc::new(crate::execution_fixture::Gate::default()),
        1,
    ));
    session.turns.insert(turn_id.clone(), turn);
    session.turn_order.push(turn_id.clone());
    {
        let mut state = lock_state(&kernel.inner);
        state.sessions.insert(session_id.clone(), session);
        enqueue(&mut state, session_id.clone(), turn_id.clone());
    }
    let healthy_id = SessionId::new("healthy-session").unwrap();
    let healthy_header = SessionHeader::new_local(
        healthy_id.clone(),
        1,
        "/workspace",
        AgentPresetId::new("test").unwrap(),
        FrozenAgentSettings::new(
            "test",
            "system",
            rsi_ai_protocol::ModelRef::new("test", "test").unwrap(),
            rsi_sandbox::SandboxMode::ReadOnly,
            false,
        )
        .unwrap(),
    )
    .unwrap();
    let healthy_pin = ClaimFixture
        .pin(healthy_header.agent_preset_id(), None)
        .await
        .unwrap();
    let mut healthy = SessionRuntime::new(healthy_header, healthy_pin, 1, false);
    healthy
        .turns
        .insert(turn_id.clone(), TurnControl::new(1, 1));
    healthy.turn_order.push(turn_id.clone());
    {
        let mut state = lock_state(&kernel.inner);
        state.sessions.insert(healthy_id.clone(), healthy);
        enqueue(&mut state, healthy_id.clone(), turn_id.clone());
    }
    let claim = tokio::time::timeout(
        Duration::from_secs(2),
        kernel.claim("binding-worker", CancellationToken::new()),
    )
    .await
    .unwrap()
    .unwrap()
    .unwrap();
    assert_eq!(claim.session_id(), &healthy_id);
    {
        let mut state = lock_state(&kernel.inner);
        assert!(state.claim_queue.is_empty());
        assert!(
            !state
                .queued
                .contains(&(session_id.clone(), turn_id.clone()))
        );
        assert_eq!(state.next_claim, 1);
        let session = state.sessions.get_mut(&session_id).unwrap();
        assert!(
            session
                .permanent_flush_error
                .as_ref()
                .unwrap()
                .contains("quarantined")
        );
        assert!(session.flush_status.borrow().permanent_error.is_some());
        let turn = session.turns.get(&turn_id).unwrap();
        assert!(turn.claim.is_none());
        assert!(turn.prepared_lane.is_none());
    }
    drop(lane);
    assert_eq!(pool.available_permits(), 1);
}

fn tool_intent(turn_id: &TurnId, suffix: &str, parallel_safe: bool) -> SessionFactBody {
    SessionFactBody::ToolIntent {
        origin: rsi_agent_session_protocol::ToolOrigin::Model {
            effect_id: rsi_agent_session_protocol::EffectId::new("source-model").unwrap(),
        },

        program_role: rsi_tools_protocol::ToolProgramRole::Unavailable,
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
    turn.tool_source = Some(Arc::new(tool_origin::tests::source(&[
        ("call-first", "tool_first"),
        ("call-second", "tool_second"),
    ])));

    apply_tool_body(&mut turn, &first).unwrap();
    apply_tool_body(&mut turn, &second).unwrap();
    assert_eq!(turn.effects.len(), 2);
    assert!(matches!(
        apply_tool_body(&mut turn, &exclusive),
        Err(TurnError::Invalid(message))
            if message.contains("parallel-safe definitions")
    ));

    let mut turn = TurnControl::new(1, 1);
    turn.tool_source = Some(Arc::new(tool_origin::tests::source(&[(
        "call-exclusive",
        "tool_exclusive",
    )])));
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
            delivery: rsi_agent_session_protocol::MessageDelivery::NextStep,
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

#[tokio::test]
async fn captured_durability_survives_resident_sender_eviction() {
    let (status, receiver) = watch::channel(FlushStatus {
        durable_seq: 0,
        permanent_error: None,
    });
    let wait = DurabilityWait {
        status: receiver,
        through_seq: 1,
    };
    // A fast executor may finish and evict the resident Session before the
    // submitting caller first polls its durability barrier.
    status.send_replace(FlushStatus {
        durable_seq: 2,
        permanent_error: None,
    });
    drop(status);
    assert_eq!(wait.wait(&CancellationToken::new()).await.unwrap(), 2);
}

#[tokio::test]
async fn closed_flush_owner_without_commit_still_fails() {
    let (status, receiver) = watch::channel(FlushStatus {
        durable_seq: 0,
        permanent_error: None,
    });
    let wait = DurabilityWait {
        status: receiver,
        through_seq: 1,
    };
    drop(status);
    assert!(matches!(
        wait.wait(&CancellationToken::new()).await,
        Err(KernelError::Shutdown(_))
    ));
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
    assert!(matches!(
        admission
            .acquire(&SessionId::new("new-after-close").unwrap())
            .await,
        Err(TurnError::ShuttingDown)
    ));
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

#[tokio::test(start_paused = true)]
async fn pair_admission_makes_progress_without_timeout_at_full_capacity() {
    use std::future::Future;
    use std::task::{Context, Poll, Waker};
    let admission = SubmissionAdmission::new();
    let occupied = admission
        .slots
        .clone()
        .acquire_many_owned(u32::try_from(MAXIMUM_ACTIVE_SESSIONS).unwrap())
        .await
        .unwrap();
    let pairs: Vec<_> = (0..MAXIMUM_ACTIVE_SESSIONS)
        .map(|i| {
            (
                SessionId::new(format!("a-{i}")).unwrap(),
                SessionId::new(format!("z-{i}")).unwrap(),
            )
        })
        .collect();
    let mut waiting: Vec<_> = pairs
        .iter()
        .map(|(a, b)| {
            Box::pin(tokio::task::unconstrained(
                admission.acquire_pair(a, Some(b)),
            ))
        })
        .collect();
    let mut context = Context::from_waker(Waker::noop());
    for waiter in &mut waiting {
        assert!(waiter.as_mut().poll(&mut context).is_pending());
    }
    drop(occupied);
    for waiter in &mut waiting {
        assert!(
            matches!(waiter.as_mut().poll(&mut context), Poll::Ready(Ok(_))),
            "a whole operation must progress without its admission timeout"
        );
    }
    drop(waiting);
    assert_eq!(admission.slots.available_permits(), MAXIMUM_ACTIVE_SESSIONS);
}

#[tokio::test(start_paused = true)]
async fn pending_admission_is_bounded_and_dropped_waiters_remove_keys() {
    use std::future::Future;
    use std::task::{Context, Waker};
    let admission = SubmissionAdmission::new();
    let occupied = admission
        .slots
        .clone()
        .acquire_many_owned(u32::try_from(MAXIMUM_ACTIVE_SESSIONS).unwrap())
        .await
        .unwrap();
    let ids: Vec<_> = (0..MAXIMUM_PENDING_SUBMISSIONS)
        .map(|i| SessionId::new(format!("pending-{i}")).unwrap())
        .collect();
    let mut waiting: Vec<_> = ids
        .iter()
        .map(|id| Box::pin(tokio::task::unconstrained(admission.acquire(id))))
        .collect();
    let mut context = Context::from_waker(Waker::noop());
    for waiter in &mut waiting {
        assert!(waiter.as_mut().poll(&mut context).is_pending());
    }
    let excess = SessionId::new("excess").unwrap();
    assert!(matches!(
        admission.acquire(&excess).await,
        Err(TurnError::Capacity)
    ));
    assert!(!admission.sessions.lock().unwrap().contains_key(&excess));
    assert_eq!(
        admission.sessions.lock().unwrap().len(),
        MAXIMUM_PENDING_SUBMISSIONS
    );
    drop(waiting.pop());
    assert_eq!(admission.pending.available_permits(), 1);
    assert_eq!(
        admission.sessions.lock().unwrap().len(),
        MAXIMUM_PENDING_SUBMISSIONS - 1
    );
    drop(waiting);
    assert!(admission.sessions.lock().unwrap().is_empty());
    assert_eq!(
        admission.pending.available_permits(),
        MAXIMUM_PENDING_SUBMISSIONS
    );
    drop(occupied);
}

#[tokio::test(start_paused = true)]
async fn pair_admission_shares_one_deadline_and_deduplicates_keys() {
    use std::future::Future;
    use std::task::{Context, Poll, Waker};
    let admission = SubmissionAdmission::new();
    let a = SessionId::new("a").unwrap();
    let z = SessionId::new("z").unwrap();
    let held = admission.acquire(&z).await.unwrap();
    let occupied = admission
        .slots
        .clone()
        .acquire_many_owned(u32::try_from(MAXIMUM_ACTIVE_SESSIONS - 1).unwrap())
        .await
        .unwrap();
    let mut pair = Box::pin(tokio::task::unconstrained(
        admission.acquire_pair(&z, Some(&a)),
    ));
    let mut context = Context::from_waker(Waker::noop());
    assert!(pair.as_mut().poll(&mut context).is_pending());
    tokio::time::advance(
        DURABILITY_WAIT_TIMEOUT
            .checked_sub(Duration::from_secs(1))
            .unwrap(),
    )
    .await;
    // Keep capacity full while allowing the second keyed lock to advance.
    drop(held);
    let last = admission.slots.clone().try_acquire_owned().unwrap();
    assert!(pair.as_mut().poll(&mut context).is_pending());
    tokio::time::advance(Duration::from_secs(1)).await;
    assert!(matches!(
        pair.as_mut().poll(&mut context),
        Poll::Ready(Err(TurnError::Capacity))
    ));
    drop(pair);
    assert!(admission.sessions.lock().unwrap().is_empty());
    drop((last, occupied));
    let duplicate = admission.acquire_pair(&a, Some(&a)).await.unwrap();
    assert_eq!(
        admission.slots.available_permits(),
        MAXIMUM_ACTIVE_SESSIONS - 1
    );
    assert_eq!(admission.sessions.lock().unwrap().len(), 1);
    drop(duplicate);
    assert!(admission.sessions.lock().unwrap().is_empty());
}

#[test]
fn retired_submission_key_cannot_remove_its_replacement() {
    let admission = SubmissionAdmission::new();
    let id = SessionId::new("reused").unwrap();
    let old = admission.register(&id);
    let weak = Arc::downgrade(&old);
    let mut sessions = admission.sessions.lock().unwrap();
    let retired = std::thread::spawn(move || drop(old));
    // Last-owner Drop must now wait for our registry guard.
    while weak.strong_count() != 0 {
        std::thread::yield_now();
    }
    let replacement = Arc::new(SubmissionKey {
        id: id.clone(),
        mutex: Arc::new(AsyncMutex::new(())),
        registry: Arc::downgrade(&admission.sessions),
    });
    sessions.insert(id.clone(), Arc::downgrade(&replacement));
    drop(sessions);
    retired.join().unwrap();
    assert!(
        admission
            .sessions
            .lock()
            .unwrap()
            .get(&id)
            .unwrap()
            .ptr_eq(&Arc::downgrade(&replacement))
    );
    drop(replacement);
    assert!(admission.sessions.lock().unwrap().is_empty());
}
