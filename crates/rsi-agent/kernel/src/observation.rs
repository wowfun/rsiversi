use super::*;

pub(super) async fn read_facts_bounded(
    inner: &Arc<KernelInner>,
    session_id: &SessionId,
    after_seq: u64,
    requested_limit: usize,
) -> std::result::Result<rsi_agent_store_protocol::StoreFactPage, StoreError> {
    let (effective_limit, permit) = acquire_store_read(inner, requested_limit).await?;
    let result = inner
        .store
        .read_facts(session_id, after_seq, effective_limit)
        .await;
    drop(permit);
    result
}

pub(super) async fn read_controls_bounded(
    inner: &Arc<KernelInner>,
    session_id: &SessionId,
    after_seq: u64,
    requested_limit: usize,
) -> std::result::Result<rsi_agent_store_protocol::StoreControlPage, StoreError> {
    let (effective_limit, permit) = acquire_store_read(inner, requested_limit).await?;
    let result = inner
        .store
        .read_controls(session_id, after_seq, effective_limit)
        .await;
    drop(permit);
    result
}

pub(super) async fn read_domain_states_bounded(
    inner: &Arc<KernelInner>,
    session_id: &SessionId,
    at_control_seq: Option<u64>,
) -> std::result::Result<rsi_agent_store_protocol::StoreDomainStatePage, StoreError> {
    // A complete domain set and its decode scratch fit below the existing single-record reservation.
    let (_, permit) = acquire_store_read(inner, 1).await?;
    let result = inner
        .store
        .read_domain_states(session_id, at_control_seq)
        .await;
    drop(permit);
    let page = result?;
    page.validate()?;
    Ok(page)
}

pub(super) async fn read_domain_request_bounded(
    inner: &Arc<KernelInner>,
    session_id: &SessionId,
    request_id: &rsi_agent_session_protocol::DomainRequestId,
) -> TurnResult<Option<rsi_agent_turn_protocol::DomainMutationReceipt>> {
    let (_, permit) = acquire_store_read(inner, 1)
        .await
        .map_err(turn_store_error)?;
    let result = inner
        .store
        .read_domain_request(session_id, request_id)
        .await;
    drop(permit);
    let Some(record) = result.map_err(turn_store_error)? else {
        return Ok(None);
    };
    let receipt = rsi_agent_turn_protocol::DomainMutationReceipt::new(session_id.clone(), record)?;
    if receipt.commit().request_id() != Some(request_id) {
        return Err(TurnError::Invariant(
            "domain request lookup changed the selected identity".into(),
        ));
    }
    Ok(Some(receipt))
}

pub(super) async fn read_fork_page_from_header(
    inner: &Arc<KernelInner>,
    header: &SessionHeader,
    after_parent_seq: u64,
    limit: usize,
) -> TurnResult<Option<ForkFactPage>> {
    let Some(origin) = header.fork_origin().cloned() else {
        return Ok(None);
    };
    if after_parent_seq < origin.resolved_after_seq
        || after_parent_seq > origin.resolved_terminal_seq
    {
        return Err(TurnError::Invalid(
            "fork parent cursor is outside the immutable inherited interval".into(),
        ));
    }
    if after_parent_seq == origin.resolved_after_seq {
        let parent_header = read_validated_header_bounded(inner, &origin.parent_session_id)
            .await
            .map_err(turn_store_error)?;
        if parent_header
            .fingerprint()
            .map_err(|error| TurnError::Invalid(error.to_string()))?
            != origin.parent_header_fingerprint
        {
            return Err(TurnError::Invariant(
                "fork parent Header fingerprint changed".into(),
            ));
        }
        let boundary = inner
            .store
            .resolve_fork_boundary(
                &origin.parent_session_id,
                &origin.invoking_turn_id,
                origin.requested_turns.clone(),
            )
            .await
            .map_err(turn_store_error)?;
        if boundary.resolved_after_seq != origin.resolved_after_seq
            || boundary.resolved_terminal_seq != origin.resolved_terminal_seq
            || boundary.terminal_prefix_sha256 != origin.terminal_prefix_sha256
            || boundary.resolved_terminal_control_seq != origin.resolved_terminal_control_seq
            || boundary.terminal_control_prefix_sha256 != origin.terminal_control_prefix_sha256
            || boundary.effective_turns != origin.effective_turns
        {
            return Err(TurnError::Invariant(
                "fork parent boundary changed after child creation".into(),
            ));
        }
    }
    if after_parent_seq == origin.resolved_terminal_seq {
        return Ok(Some(ForkFactPage {
            facts: Vec::new(),
            through_parent_seq: after_parent_seq,
            terminal_parent_seq: origin.resolved_terminal_seq,
        }));
    }
    let page = read_facts_bounded(inner, &origin.parent_session_id, after_parent_seq, limit)
        .await
        .map_err(turn_store_error)?;
    let facts = page
        .facts
        .into_iter()
        .take_while(|fact| fact.seq() <= origin.resolved_terminal_seq)
        .map(Arc::new)
        .collect::<Vec<_>>();
    let through_parent_seq = facts
        .last()
        .map_or(after_parent_seq, |fact| fact.seq())
        .min(origin.resolved_terminal_seq);
    if through_parent_seq == after_parent_seq {
        return Err(TurnError::Invariant(
            "fork parent Fact page made no progress".into(),
        ));
    }
    Ok(Some(ForkFactPage {
        facts,
        through_parent_seq,
        terminal_parent_seq: origin.resolved_terminal_seq,
    }))
}

pub(super) async fn observe_agent_wait_change(
    kernel: &AgentKernel,
    caller: &AgentCallerAuthority,
    baseline: &StoreAgentSubtreeSnapshot,
) -> TurnResult<Option<WaitResumeCause>> {
    kernel.validate_agent_caller(caller)?;
    let current = kernel
        .inner
        .store
        .read_agent_subtree_snapshot(caller.session_id())
        .await
        .map_err(turn_store_error)?;
    current.validate().map_err(turn_store_error)?;
    if current.descendants.len() != baseline.descendants.len()
        || current
            .descendants
            .iter()
            .zip(&baseline.descendants)
            .any(|(current, previous)| current.status.session_id != previous.status.session_id)
    {
        kernel.validate_agent_caller(caller)?;
        return Ok(Some(WaitResumeCause::Message));
    }
    for (current, previous) in current.descendants.iter().zip(&baseline.descendants) {
        if current.status.durable_control_seq < previous.status.durable_control_seq {
            return Err(TurnError::Invariant(format!(
                "descendant control watermark regressed for `{}`",
                current.status.session_id
            )));
        }
        if current.status.durable_control_seq > previous.status.durable_control_seq {
            let mut cursor = previous.status.durable_control_seq;
            let mut settled = false;
            while cursor < current.status.durable_control_seq {
                let controls = read_controls_bounded(
                    &kernel.inner,
                    &current.status.session_id,
                    cursor,
                    MAXIMUM_FACTS_PER_READ,
                )
                .await
                .map_err(turn_store_error)?;
                let mut through = cursor;
                for record in controls
                    .records
                    .iter()
                    .take_while(|record| record.seq() <= current.status.durable_control_seq)
                {
                    through = record.seq();
                    settled |= matches!(
                        record.body(),
                        AgentControlRecordBody::ActivationSettled { .. }
                    );
                }
                if settled {
                    break;
                }
                if through == cursor {
                    return Err(TurnError::Invariant(format!(
                        "descendant control scan made no progress for `{}`",
                        current.status.session_id
                    )));
                }
                cursor = through;
            }
            kernel.validate_agent_caller(caller)?;
            return Ok(Some(if settled {
                WaitResumeCause::Completion
            } else {
                WaitResumeCause::Message
            }));
        }
    }
    let mailbox = scan_durable_messages(&kernel.inner, caller.session_id(), None).await?;
    kernel.validate_agent_caller(caller)?;
    Ok(mailbox
        .pending
        .iter()
        .find(|entry| entry.target == MessageTarget::NextStep)
        .map(|entry| {
            if matches!(entry.message.source, AgentMessageSource::Completion { .. }) {
                WaitResumeCause::Completion
            } else {
                WaitResumeCause::Message
            }
        }))
}

pub(super) async fn scan_durable_messages(
    inner: &Arc<KernelInner>,
    session_id: &SessionId,
    selected_id: Option<&MessageId>,
) -> TurnResult<DurableMessageScan> {
    // One mailbox page contains at most 32 MiB of pending message payload plus
    // one selected message and bounded index metadata, below one maximum-Fact
    // reservation. Keep that reservation through decoding and validation.
    let permit = acquire_store_read_bytes(inner, MAXIMUM_SESSION_FACT_BYTES)
        .await
        .map_err(turn_store_error)?;
    let mailbox = inner
        .store
        .read_agent_mailbox(session_id, selected_id)
        .await
        .map_err(turn_store_error);
    drop(permit);
    let mailbox = mailbox?;
    let pending_count = mailbox.pending_count;
    let pending = mailbox
        .pending
        .into_iter()
        .map(durable_message_entry)
        .collect::<Vec<_>>();
    Ok(DurableMessageScan {
        selected: mailbox.selected.map(durable_message_entry),
        pending_count,
        pending,
        durable_control_seq: mailbox.durable_control_seq,
        durable_fact_seq: mailbox.durable_fact_seq,
    })
}

pub(super) fn bounded_step_message_prefix(
    pending: Vec<DurableMessageEntry>,
    maximum_payload_bytes: usize,
) -> TurnResult<Vec<DurableMessageEntry>> {
    let mut selected = Vec::new();
    let mut bytes = 0_usize;
    for entry in pending {
        let projected = bytes
            .checked_add(entry.encoded_message_bytes)
            .ok_or_else(|| TurnError::Invalid("message batch size overflowed".into()))?;
        if !selected.is_empty() && projected > maximum_payload_bytes {
            break;
        }
        if projected > maximum_payload_bytes {
            return Err(TurnError::Capacity);
        }
        bytes = projected;
        selected.push(entry);
    }
    Ok(selected)
}

pub(super) fn durable_message_entry(entry: StoreAgentMessage) -> DurableMessageEntry {
    let state = match entry.state {
        StoreAgentMessageState::Pending => MessageState::Pending,
        StoreAgentMessageState::Claimed {
            activation_id,
            turn_id,
            step_id,
            entered_fact_seq,
        } => MessageState::Claimed {
            activation_id,
            turn_id,
            step_id,
            entered_fact_seq,
        },
        StoreAgentMessageState::Discarded {
            reason,
            control_seq,
        } => MessageState::Discarded {
            reason,
            control_seq,
        },
    };
    DurableMessageEntry {
        delivery: entry.delivery,
        message: entry.message,
        encoded_message_bytes: entry.encoded_message_bytes,
        root_session_id: entry.root_session_id,
        target: entry.target,
        wake_required: entry.wake_required,
        accepted_control_seq: entry.accepted_control_seq,
        state,
    }
}

pub(super) fn message_receipt(
    session_id: &SessionId,
    observed_fact_seq: u64,
    entry: &DurableMessageEntry,
) -> MessageReceipt {
    MessageReceipt {
        session_id: session_id.clone(),
        message_id: entry.message.message_id.clone(),
        accepted_control_seq: entry.accepted_control_seq,
        observed_fact_seq,
        state: entry.state.clone(),
    }
}

pub(super) fn entered_message_source(message: &AgentMessage) -> InputMessageSource {
    match &message.source {
        AgentMessageSource::Human => InputMessageSource::Human {
            message_id: message.message_id.clone(),
        },
        AgentMessageSource::Agent { source_session_id } => InputMessageSource::Agent {
            message_id: message.message_id.clone(),
            source_session_id: source_session_id.clone(),
        },
        AgentMessageSource::Completion {
            child_session_id,
            activation_id,
        } => InputMessageSource::Completion {
            message_id: message.message_id.clone(),
            child_session_id: child_session_id.clone(),
            activation_id: activation_id.clone(),
        },
    }
}

pub(super) fn activation_terminal_controls(
    expected_control_seq: u64,
    timestamp_ms: u64,
    terminal: AgentControlRecordBody,
    pending_promotable_message_ids: &[MessageId],
) -> TurnResult<Vec<AgentControlRecord>> {
    let mut controls = Vec::with_capacity(pending_promotable_message_ids.len().saturating_add(1));
    for message_id in pending_promotable_message_ids {
        let seq = expected_control_seq
            .checked_add(
                u64::try_from(controls.len())
                    .map_err(|_| TurnError::Invariant("control offset exceeds u64".into()))?
                    .checked_add(1)
                    .ok_or_else(|| TurnError::Invariant("control offset exhausted".into()))?,
            )
            .ok_or_else(|| TurnError::Invariant("control sequence exhausted".into()))?;
        controls.push(
            AgentControlRecord::new(
                seq,
                timestamp_ms,
                AgentControlRecordBody::MessagePromoted {
                    message_id: message_id.clone(),
                },
            )
            .map_err(|error| TurnError::Invalid(error.to_string()))?,
        );
    }
    let seq = expected_control_seq
        .checked_add(
            u64::try_from(controls.len())
                .map_err(|_| TurnError::Invariant("control offset exceeds u64".into()))?
                .checked_add(1)
                .ok_or_else(|| TurnError::Invariant("control offset exhausted".into()))?,
        )
        .ok_or_else(|| TurnError::Invariant("control sequence exhausted".into()))?;
    controls.push(
        AgentControlRecord::new(seq, timestamp_ms, terminal)
            .map_err(|error| TurnError::Invalid(error.to_string()))?,
    );
    Ok(controls)
}

pub(super) fn activation_outcome(outcome: &TurnOutcome) -> ActivationOutcome {
    match outcome {
        TurnOutcome::Completed => ActivationOutcome::Completed,
        TurnOutcome::Cancelled => ActivationOutcome::Cancelled,
        TurnOutcome::Failed { code, message }
        | TurnOutcome::PartialFailed { code, message, .. } => ActivationOutcome::Failed {
            code: code.clone(),
            message: message.clone(),
        },
        TurnOutcome::Interrupted { reason, .. } => ActivationOutcome::Failed {
            code: "turn.interrupted".into(),
            message: reason.clone(),
        },
        TurnOutcome::BudgetExceeded {
            dimension,
            consumed,
            limit,
        } => ActivationOutcome::Failed {
            code: "turn.budget_exceeded".into(),
            message: bounded_diagnostic(&format!(
                "{dimension:?} consumed {consumed} with limit {limit}"
            )),
        },
    }
}

pub(super) fn completion_message(outcome: &TurnOutcome) -> String {
    match outcome {
        TurnOutcome::Completed => "Subagent activation completed.".into(),
        TurnOutcome::Cancelled => "Subagent activation was cancelled.".into(),
        TurnOutcome::Failed { message, .. } | TurnOutcome::PartialFailed { message, .. } => {
            bounded_diagnostic(&format!("Subagent activation failed: {message}"))
        }
        TurnOutcome::Interrupted { reason, .. } => {
            bounded_diagnostic(&format!("Subagent activation was interrupted: {reason}"))
        }
        TurnOutcome::BudgetExceeded { .. } => {
            "Subagent activation exhausted its frozen Turn budget.".into()
        }
    }
}

pub(super) fn completion_message_id(
    child_session_id: &SessionId,
    activation_id: &rsi_agent_session_protocol::ActivationId,
) -> TurnResult<MessageId> {
    let mut digest = Sha256::new();
    digest.update(child_session_id.as_str().as_bytes());
    digest.update([0]);
    digest.update(activation_id.as_str().as_bytes());
    let digest = digest.finalize();
    MessageId::new(format!("completion-{digest:x}"))
        .map_err(|error| TurnError::Invariant(error.to_string()))
}

pub(super) fn agent_root_and_path(header: &SessionHeader) -> (SessionId, AgentPath) {
    header.fork_origin().map_or_else(
        || (header.session_id().clone(), AgentPath::root()),
        |origin| (origin.root_session_id.clone(), origin.path.clone()),
    )
}

pub(super) async fn list_direct_agent_children(
    store: &Arc<dyn SessionStore>,
    parent_session_id: &SessionId,
) -> TurnResult<Vec<StoreAgentChild>> {
    let mut after = None;
    let mut children = Vec::new();
    loop {
        let page = store
            .list_agent_children(parent_session_id, after.as_ref(), MAXIMUM_SESSIONS_PER_READ)
            .await
            .map_err(turn_store_error)?;
        page.validate().map_err(turn_store_error)?;
        children.extend(page.children);
        if children.len() > MAXIMUM_DURABLE_AGENT_TREE_NODES {
            return Err(TurnError::Invariant(
                "durable Agent tree exceeds its node bound".into(),
            ));
        }
        if !page.has_more {
            return Ok(children);
        }
        after = children.last().map(|child| child.session_id.clone());
        if after.is_none() {
            return Err(TurnError::Invariant(
                "Agent-child enumeration made no progress".into(),
            ));
        }
    }
}

pub(super) async fn list_agent_descendants(
    store: &Arc<dyn SessionStore>,
    parent_session_id: &SessionId,
) -> TurnResult<Vec<(SessionId, StoreAgentChild)>> {
    let mut pending = VecDeque::from([parent_session_id.clone()]);
    let mut descendants = Vec::new();
    while let Some(parent) = pending.pop_front() {
        for child in list_direct_agent_children(store, &parent).await? {
            if descendants.len() >= MAXIMUM_DURABLE_AGENT_TREE_NODES - 1 {
                return Err(TurnError::Capacity);
            }
            pending.push_back(child.session_id.clone());
            descendants.push((parent.clone(), child));
        }
    }
    descendants.sort_by(|(_, left), (_, right)| left.path.cmp(&right.path));
    Ok(descendants)
}

pub(super) async fn descendant_session_ids(
    store: &Arc<dyn SessionStore>,
    parent_session_id: &SessionId,
) -> TurnResult<Vec<SessionId>> {
    let snapshot = store
        .read_agent_subtree_snapshot(parent_session_id)
        .await
        .map_err(turn_store_error)?;
    snapshot.validate().map_err(turn_store_error)?;
    Ok(snapshot
        .descendants
        .into_iter()
        .map(|descendant| descendant.status.session_id)
        .collect())
}

pub(super) async fn control_tail(
    inner: &Arc<KernelInner>,
    session_id: &SessionId,
) -> TurnResult<u64> {
    Ok(read_controls_bounded(inner, session_id, 0, 1)
        .await
        .map_err(turn_store_error)?
        .durable_seq)
}

pub(super) async fn read_turn_facts_bounded(
    inner: &KernelInner,
    session_id: &SessionId,
    turn_id: &TurnId,
    after_seq: u64,
    requested_limit: usize,
) -> std::result::Result<rsi_agent_store_protocol::StoreTurnFactPage, StoreError> {
    let (effective_limit, permit) = acquire_store_read(inner, requested_limit).await?;
    let result = inner
        .store
        .read_turn_facts(session_id, turn_id, after_seq, effective_limit)
        .await;
    drop(permit);
    result
}

pub(super) async fn read_turn_boundary_bounded(
    inner: &KernelInner,
    session_id: &SessionId,
    turn_id: &TurnId,
) -> std::result::Result<rsi_agent_store_protocol::StoreTurnBoundary, StoreError> {
    let permit = acquire_store_read_bytes(inner, MAXIMUM_SESSION_FACT_BYTES).await?;
    let result = inner.store.read_turn_boundary(session_id, turn_id).await;
    drop(permit);
    result
}

pub(super) async fn read_validated_header_bounded(
    inner: &KernelInner,
    session_id: &SessionId,
) -> std::result::Result<SessionHeader, StoreError> {
    let permit = acquire_store_read_bytes(inner, MAXIMUM_SESSION_HEADER_BYTES).await?;
    inner.store.validate_session(session_id).await?;
    let result = inner.store.header(session_id).await;
    drop(permit);
    result
}

pub(super) async fn acquire_store_read(
    inner: &KernelInner,
    requested_limit: usize,
) -> std::result::Result<(usize, tokio::sync::OwnedSemaphorePermit), StoreError> {
    let (effective_limit, reservation) = if requested_limit == 1 {
        (1, MAXIMUM_SESSION_FACT_BYTES)
    } else if inner.limits.maximum_store_read_bytes >= MAXIMUM_STORE_BATCH_BYTES {
        (requested_limit, MAXIMUM_STORE_BATCH_BYTES)
    } else {
        (1, MAXIMUM_SESSION_FACT_BYTES)
    };
    let permit = acquire_store_read_bytes(inner, reservation).await?;
    Ok((effective_limit, permit))
}

pub(super) async fn acquire_store_read_bytes(
    inner: &KernelInner,
    reservation: usize,
) -> std::result::Result<tokio::sync::OwnedSemaphorePermit, StoreError> {
    let permits = u32::try_from(reservation).map_err(|_| {
        StoreError::Invalid("Store-read reservation exceeds semaphore representation".into())
    })?;
    Arc::clone(&inner.store_read_admission)
        .acquire_many_owned(permits)
        .await
        .map_err(|_| StoreError::Io("Kernel Store-read admission closed".into()))
}

pub(super) const fn context_checkpoints_enabled(inner: &KernelInner) -> bool {
    inner.limits.maximum_store_read_bytes >= MAXIMUM_CONTEXT_CHECKPOINT_BYTES
}

pub(super) async fn read_observed_facts(
    inner: &Arc<KernelInner>,
    session: &SessionId,
    cursor: u64,
) -> TurnResult<(Vec<ObservedFact>, u64)> {
    let requested = if inner.limits.maximum_retained_observation_bytes < MAXIMUM_STORE_BATCH_BYTES {
        1
    } else {
        MAXIMUM_FACTS_PER_READ
    };
    let (limit, _read) = acquire_store_read(inner, requested)
        .await
        .map_err(turn_store_error)?;
    let page = inner
        .store
        .read_facts(session, cursor, limit)
        .await
        .map_err(turn_store_error)?;
    if page.after_seq != cursor || page.facts.len() > limit {
        return Err(TurnError::Invariant(
            "Fact observation page differs from its requested cursor or limit".into(),
        ));
    }
    page.validate()
        .map_err(|error| TurnError::Invariant(bounded_diagnostic(&error.to_string())))?;
    if page.facts.is_empty() && page.durable_seq > cursor {
        return Err(TurnError::Invariant(
            "durable observation page made no progress".into(),
        ));
    }
    let retained = inner
        .observation_retention
        .retain_facts(page.facts.into_iter().map(Arc::new).collect())?;
    Ok((retained, page.durable_seq))
}

async fn read_observed_controls(
    inner: &Arc<KernelInner>,
    session: &SessionId,
    cursor: u64,
) -> TurnResult<(Vec<ObservedControl>, u64)> {
    let requested = if inner.limits.maximum_retained_observation_bytes < MAXIMUM_STORE_BATCH_BYTES {
        1
    } else {
        MAXIMUM_FACTS_PER_READ
    };
    let (limit, _read) = acquire_store_read(inner, requested)
        .await
        .map_err(turn_store_error)?;
    let page = inner
        .store
        .read_controls(session, cursor, limit)
        .await
        .map_err(turn_store_error)?;
    if page.after_seq != cursor || page.records.len() > limit {
        return Err(TurnError::Invariant(
            "control observation page differs from its requested cursor or limit".into(),
        ));
    }
    page.validate()
        .map_err(|error| TurnError::Invariant(bounded_diagnostic(&error.to_string())))?;
    if page.records.is_empty() && page.durable_seq > cursor {
        return Err(TurnError::Invariant(
            "control observation page made no progress".into(),
        ));
    }
    let retained = inner
        .observation_retention
        .retain_controls(page.records.into_iter().map(Arc::new).collect())?;
    Ok((retained, page.durable_seq))
}

pub(super) async fn fill_observation_page(
    inner: &Arc<KernelInner>,
    state: &mut DurableObservationState,
) -> TurnResult<()> {
    while state.pending.is_empty() && (state.read_controls || state.read_facts) {
        let controls = if matches!(state.next_page, ObservationPageKind::Control) {
            state.read_controls
        } else {
            !state.read_facts
        };
        if controls {
            let (records, durable_control_seq) =
                read_observed_controls(inner, &state.session_id, state.control_seq).await?;
            if let Some(last) = records.last() {
                state.control_seq = last.seq();
            }
            state.read_controls = state.control_seq < durable_control_seq;
            state.pending.extend(
                records
                    .into_iter()
                    .map(|record| SessionObservation::Control {
                        record,
                        durable_control_seq,
                    }),
            );
        } else {
            let (facts, durable_fact_seq) =
                read_observed_facts(inner, &state.session_id, state.fact_seq).await?;
            if let Some(last) = facts.last() {
                state.fact_seq = last.seq();
            }
            state.read_facts = state.fact_seq < durable_fact_seq;
            state
                .pending
                .extend(facts.into_iter().map(|fact| SessionObservation::Fact {
                    fact,
                    durable_fact_seq,
                }));
        }
        state.next_page = if controls {
            ObservationPageKind::Fact
        } else {
            ObservationPageKind::Control
        };
    }
    Ok(())
}

pub(super) async fn durable_observation_next(
    mut state: DurableObservationState,
) -> Option<(TurnResult<SessionObservation>, DurableObservationState)> {
    loop {
        if state.stopped {
            return None;
        }
        if let Some(observation) = state.pending.pop_front() {
            return Some((Ok(observation), state));
        }
        let inner = state.inner.upgrade()?;
        if state.watch.has_changed() {
            state.watch.mark_seen();
            state.read_controls = true;
            state.read_facts = true;
        }
        if !state.read_controls && !state.read_facts {
            tokio::select! {
                () = inner.stop_worker.cancelled() => return None,
                () = state.watch.changed() => {}
                () = tokio::time::sleep(DURABLE_OBSERVER_FALLBACK_INTERVAL) => {}
            }
            state.watch.mark_seen();
            state.read_controls = true;
            state.read_facts = true;
        }
        if let Err(error) = fill_observation_page(&inner, &mut state).await {
            state.stopped = true;
            return Some((Err(error), state));
        }
    }
}

pub(super) async fn observation_next(
    mut state: ObservationState,
) -> Option<(TurnResult<TurnUpdate>, ObservationState)> {
    if state.ended {
        return None;
    }
    loop {
        let previous_durable = state.durable_target;
        let current = *state.receiver.borrow_and_update();
        state.live_target = state.live_target.max(current.live_seq);
        state.durable_target = state.durable_target.max(current.durable_seq);
        if state.cursor < state.durable_target {
            if let Some(update) = take_buffered_durable_fact(&mut state) {
                return Some((update, state));
            }
            let inner = state.inner.upgrade()?;
            match read_observed_facts(&inner, &state.session_id, state.cursor).await {
                Ok((facts, durable_seq)) => {
                    state.durable_target = state.durable_target.max(durable_seq);
                    state.live_target = state.live_target.max(durable_seq);
                    state.durable_facts = facts.into();
                    if state.durable_facts.is_empty() {
                        state.ended = true;
                        return Some((
                            Err(TurnError::Invariant(
                                "durable observation cursor made no progress".into(),
                            )),
                            state,
                        ));
                    }
                    continue;
                }
                Err(error) => {
                    state.ended = true;
                    return Some((Err(error), state));
                }
            }
        }
        if state.durable_target > previous_durable && state.cursor >= state.durable_target {
            return Some((
                Ok(TurnUpdate::Durable {
                    durable_seq: state.durable_target,
                }),
                state,
            ));
        }
        if state.cursor < state.live_target
            && let Some(fact) = next_speculative_observation_fact(&mut state)
        {
            let inner = state.inner.upgrade()?;
            let fact = match inner.observation_retention.retain_fact(fact) {
                Ok(fact) => fact,
                Err(error) => {
                    state.ended = true;
                    return Some((Err(error), state));
                }
            };
            state.cursor = fact.seq();
            return Some((
                Ok(TurnUpdate::Fact {
                    fact,
                    durable_seq: state.durable_target,
                }),
                state,
            ));
        }
        let permanent_error = state
            .flush_status
            .as_ref()
            .and_then(|status| status.borrow().permanent_error.clone());
        if let Some(error) = permanent_error {
            let update = observation_flush_result(&mut state, error);
            return Some((update, state));
        }
        let signal = if let Some(status) = state.flush_status.as_mut() {
            tokio::select! {
                update = state.receiver.changed() => ObservationSignal::Update(update),
                changed = status.changed() => ObservationSignal::Flush(changed),
            }
        } else {
            ObservationSignal::Update(state.receiver.changed().await)
        };
        match signal {
            ObservationSignal::Flush(Ok(())) | ObservationSignal::Update(Ok(())) => {
                // The next loop consumes and compares any exact unseen value.
            }
            ObservationSignal::Flush(Err(_)) => {
                state.flush_status = None;
            }
            ObservationSignal::Update(Err(_)) => return None,
        }
    }
}

pub(super) fn take_buffered_durable_fact(
    state: &mut ObservationState,
) -> Option<TurnResult<TurnUpdate>> {
    let fact = state.durable_facts.pop_front()?;
    if fact.seq() != state.cursor.saturating_add(1) || fact.seq() > state.durable_target {
        state.ended = true;
        return Some(Err(TurnError::Invariant(
            "buffered durable observation Facts are not contiguous".into(),
        )));
    }
    state.cursor = fact.seq();
    Some(Ok(TurnUpdate::Fact {
        fact,
        durable_seq: state.durable_target,
    }))
}

pub(super) fn next_speculative_observation_fact(
    state: &mut ObservationState,
) -> Option<Arc<SessionFact>> {
    let inner = state.inner.upgrade()?;
    let kernel = lock_state(&inner);
    let session = kernel.sessions.get(&state.session_id)?;
    let live_seq = session.live_seq().ok()?;
    state.live_target = state.live_target.max(live_seq);
    state.durable_target = state.durable_target.max(session.durable_seq);
    let next_seq = state.cursor.checked_add(1)?;
    let pending_start = session.durable_seq.checked_add(1)?;
    let offset = next_seq.checked_sub(pending_start)?;
    let fact = session.pending.get(usize::try_from(offset).ok()?)?;
    (fact.seq() == next_seq && (!is_terminal_fact(fact) || fact.seq() <= session.durable_seq))
        .then(|| Arc::clone(fact))
}

pub(super) fn observation_flush_result(
    state: &mut ObservationState,
    error: String,
) -> TurnResult<TurnUpdate> {
    state.ended = true;
    Err(TurnError::Flush(error))
}

pub(super) async fn read_header_bounded(
    inner: &KernelInner,
    session_id: &SessionId,
) -> std::result::Result<SessionHeader, StoreError> {
    let _permit = acquire_store_read_bytes(inner, MAXIMUM_SESSION_HEADER_BYTES).await?;
    inner.store.header(session_id).await
}
