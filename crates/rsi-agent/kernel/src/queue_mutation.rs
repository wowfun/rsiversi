//! Queue editing shares admission with both `NextTurn` and safe-boundary `NextStep` claims.
use super::*;
use rsi_agent_session_protocol::{
    MessageDelivery, QueueMutation, QueueMutationOutcome as Outcome, QueueMutationReceipt,
    QueueMutationRejection as Rejection, QueueMutationRequest, QueueOperationId,
};

impl AgentKernel {
    pub(super) async fn read_queue_receipt(
        &self,
        session: &SessionId,
        operation: &QueueOperationId,
    ) -> TurnResult<Option<QueueMutationReceipt>> {
        let operation = operation.clone();
        let (receipt, _permit, _lease) = store_reads::read(
            &self.inner,
            session,
            8192,
            true,
            move |store, id| async move { store.read_queue_mutation(&id, &operation).await },
        )
        .await
        .map_err(turn_store_error)?;
        if let Some(receipt) = &receipt {
            receipt
                .validate()
                .map_err(|error| TurnError::Invariant(error.to_string()))?;
        }
        Ok(receipt)
    }

    #[allow(clippy::too_many_lines)] // One Session admission guards preconditions, predecessor retirement, successor acceptance and its durable receipt.
    pub(super) async fn mutate_queue_owned(
        &self,
        session: &SessionId,
        request: QueueMutationRequest,
        execution: Option<rsi_execution::ExecutionLease>,
    ) -> TurnResult<QueueMutationReceipt> {
        let fingerprint = request
            .fingerprint()
            .map_err(|error| TurnError::Invalid(error.to_string()))?;
        let admission = self.inner.submission_admission.acquire(session).await?;
        if let Some(receipt) = self
            .read_queue_receipt(session, &request.operation_id)
            .await?
        {
            return if receipt.request_fingerprint == fingerprint {
                Ok(receipt)
            } else {
                Err(TurnError::QueueOperationConflict)
            };
        }
        self.fence_pending_terminal(session).await?;
        if let Some(error) = lock_state(&self.inner)
            .sessions
            .get(session)
            .and_then(|session| session.permanent_flush_error.clone())
        {
            return Err(TurnError::Flush(error));
        }
        let successor_exists = if let Some(id) = request.mutation.new_message_id().cloned() {
            let (exists, _permit, _lease) = store_reads::read(
                &self.inner,
                session,
                256,
                true,
                move |store, session| async move {
                    store.agent_message_exists(&session, &id).await
                },
            )
            .await
            .map_err(turn_store_error)?;
            exists
        } else {
            false
        };
        let watermarks = self
            .inner
            .store
            .read_watermarks(session)
            .await
            .map_err(turn_store_error)?;
        let slot = request.slot_id.clone();
        let (entry, payload_permit, validation_lease) = store_reads::read(
            &self.inner,
            session,
            MAXIMUM_SESSION_FACT_BYTES,
            true,
            move |store, id| async move { store.read_queue_slot(&id, &slot).await },
        )
        .await
        .map_err(turn_store_error)?;
        // Live Turns and flush failures stay resident; startup repairs unfinished Turns.
        // Loading a cold Session cannot recreate live conversion authority.
        let rejection = match &entry {
            None => Some(Rejection::MissingSlot),
            Some(entry) if entry.message.message_id != request.expected_message_id => {
                Some(Rejection::StaleMessage)
            }
            Some(entry) if entry.message.source != AgentMessageSource::Human => {
                Some(Rejection::NotHuman)
            }
            Some(entry) if matches!(entry.state, StoreAgentMessageState::Claimed { .. }) => {
                Some(Rejection::Claimed)
            }
            Some(entry) if matches!(entry.state, StoreAgentMessageState::Discarded { .. }) => {
                Some(Rejection::Discarded)
            }
            Some(entry) => match &request.mutation {
                QueueMutation::ConvertToSteer { .. }
                    if entry.message.options != MessageOptions::default() =>
                {
                    Some(Rejection::IncompatibleOptions)
                }
                QueueMutation::ConvertToSteer {
                    expected_turn_id, ..
                } if !lock_state(&self.inner)
                    .sessions
                    .get(session)
                    .and_then(|session| session.turns.get(expected_turn_id))
                    .is_some_and(|turn| {
                        turn.activation_id.is_some()
                            && turn.terminal.is_none()
                            && !turn.cancel_requested
                            && turn.budget_exhausted.is_none()
                    }) =>
                {
                    Some(Rejection::StaleTurn)
                }
                _ if successor_exists => Some(Rejection::MessageConflict),
                _ => None,
            },
        };
        let now = self.inner.clock.now_ms().max(1);
        let (execution_admission, execution_reservation) = if rejection.is_none()
            && let Some(successor) = request.mutation.new_message_id()
        {
            let header = read_validated_header_bounded(&self.inner, session)
                .await
                .map_err(turn_store_error)?;
            let admission = execution_admission::admit(&header, execution.as_ref())?;
            let reservation =
                self.inner
                    .execution_messages
                    .reserve(session, successor, execution.as_ref())?;
            (admission, reservation)
        } else {
            (None, None)
        };
        let mut controls = Vec::with_capacity(3);
        let next_seq = watermarks
            .durable_control_seq
            .checked_add(1)
            .ok_or_else(|| TurnError::Invariant("control sequence exhausted".into()))?;
        let record = |seq, body| {
            AgentControlRecord::new(seq, now, body)
                .map_err(|error| TurnError::Invalid(error.to_string()))
        };
        let outcome = if let Some(reason) = rejection {
            Outcome::Rejected {
                reason,
                current_message_id: entry.as_ref().map(|entry| entry.message.message_id.clone()),
            }
        } else {
            let predecessor =
                entry.expect("accepted preconditions require a pending Human predecessor");
            match &request.mutation {
                QueueMutation::Withdraw => {
                    controls.push(record(
                        next_seq,
                        AgentControlRecordBody::MessageDiscarded {
                            message_id: request.expected_message_id.clone(),
                            reason: MessageDiscardReason::Cancelled,
                        },
                    )?);
                    Outcome::Withdrawn
                }
                mutation => {
                    let mut message = predecessor.message;
                    message.message_id = mutation
                        .new_message_id()
                        .expect("successor mutation identity")
                        .clone();
                    let mut delivery = predecessor.delivery;
                    let mut target = predecessor.target;
                    // A promoted steer is now a waking input; its immutable earlier binding remains in the predecessor control.
                    let mut bound = if target == MessageTarget::NextStep {
                        predecessor.bound_turn_id
                    } else {
                        None
                    };
                    let accepted_control_seq = next_seq
                        .checked_add(1)
                        .ok_or_else(|| TurnError::Invariant("control sequence exhausted".into()))?;
                    let outcome = match mutation {
                        QueueMutation::Replace { content, .. } => {
                            message.content.clone_from(content);
                            Outcome::Replaced {
                                message_id: message.message_id.clone(),
                                accepted_control_seq,
                            }
                        }
                        QueueMutation::ConvertToSteer {
                            expected_turn_id, ..
                        } => {
                            delivery = MessageDelivery::Steer;
                            target = MessageTarget::NextStep;
                            bound = Some(expected_turn_id.clone());
                            Outcome::Converted {
                                message_id: message.message_id.clone(),
                                accepted_control_seq,
                                bound_turn_id: expected_turn_id.clone(),
                            }
                        }
                        QueueMutation::Withdraw => unreachable!(),
                    };
                    controls.push(record(
                        next_seq,
                        AgentControlRecordBody::MessageSuccessor {
                            predecessor_id: request.expected_message_id.clone(),
                            successor_id: message.message_id.clone(),
                            slot: predecessor.queue_slot,
                        },
                    )?);
                    controls.push(record(
                        accepted_control_seq,
                        AgentControlRecordBody::MessageAccepted {
                            message,
                            delivery,
                            bound_turn_id: bound,
                            root_session_id: predecessor.root_session_id,
                            target,
                            wake_required: target == MessageTarget::NextTurn,
                        },
                    )?);
                    outcome
                }
            }
        };
        let receipt = QueueMutationReceipt {
            operation_id: request.operation_id,
            request_fingerprint: fingerprint,
            slot_id: request.slot_id,
            expected_message_id: request.expected_message_id,
            control_seq: next_seq
                .checked_add(controls.len() as u64)
                .ok_or_else(|| TurnError::Invariant("control sequence exhausted".into()))?,
            outcome,
        };
        controls.push(record(
            receipt.control_seq,
            AgentControlRecordBody::QueueMutationRecorded {
                receipt: receipt.clone(),
            },
        )?);
        let kernel = self.clone();
        let session = session.clone();
        self.owned_commit(async move {
            let _admission = admission;
            let _execution_admission = execution_admission;
            let _payload = payload_permit;
            let _validation = validation_lease;
            kernel
                .commit_agent_with_flush_conflict_retry(AtomicAgentCommit {
                    sessions: vec![AtomicSessionAppend {
                        session_id: session,
                        expected_fact_seq: watermarks.durable_fact_seq,
                        expected_control_seq: watermarks.durable_control_seq,
                        header: None,
                        facts: Vec::new(),
                        controls,
                    }],
                    required_active_activations: Vec::new(),
                    quiescent_descendants_of: None,
                })
                .await?
                .map_err(turn_store_error)?;
            kernel
                .inner
                .execution_messages
                .publish(execution_reservation);
            kernel.request_ready_scan();
            Ok(receipt)
        })
        .await
    }
}
