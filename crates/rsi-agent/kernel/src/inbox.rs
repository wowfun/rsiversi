//! Native inbox wake transitions share the message acceptance transaction.
use super::*;

pub(super) enum InboxCancel {
    Clear,
    Stop { keep_inbox: bool },
}

impl AgentKernel {
    #[expect(
        clippy::too_many_lines,
        reason = "One admitted Session cut owns cancellation, inbox changes and the optional Program receipt"
    )]
    pub(super) async fn cancel_inbox(
        &self,
        id: &SessionId,
        target: InboxCancel,
        reason: Option<String>,
    ) -> TurnResult<CancelResult> {
        if let Some(reason) = &reason {
            rsi_agent_session_protocol::validate_safe_diagnostic("cancellation reason", reason)
                .map_err(|error| TurnError::Invalid(error.to_string()))?;
        }
        let keep_inbox = matches!(target, InboxCancel::Stop { keep_inbox: true });
        let stop_turns = matches!(target, InboxCancel::Stop { .. });
        let header = if keep_inbox {
            None
        } else {
            Some(self.session_header(id).await?)
        };
        let parent = match header.as_ref().and_then(SessionHeader::execution_owner) {
            Some(rsi_agent_session_protocol::ExecutionOwner::ProgramRun { session_id, .. }) => {
                Some(session_id)
            }
            _ => None,
        };
        let admission = self
            .inner
            .submission_admission
            .acquire_pair(id, parent)
            .await?;
        self.flush_resident_admitted(id).await?;
        if let Some(parent) = parent {
            self.flush_resident_admitted(parent).await?;
        }
        let inbox = self
            .inner
            .store
            .read_inbox_control_state(id, !keep_inbox)
            .await
            .map_err(turn_store_error)?;
        if stop_turns && inbox.has_open_turn {
            self.ensure_session_loaded(id).await?;
        }
        let timestamp = self.inner.clock.now_ms().max(1);
        let facts = {
            let state = lock_state(&self.inner);
            let mut facts = Vec::new();
            if stop_turns && let Some(session) = state.sessions.get(id) {
                for turn_id in &session.turn_order {
                    let Some(turn) = session.turns.get(turn_id) else {
                        continue;
                    };
                    if turn.terminal.is_some() || turn.cancel_requested {
                        continue;
                    }
                    facts.push(Arc::new(
                        SessionFact::new(
                            inbox
                                .durable_fact_seq
                                .checked_add(facts.len() as u64 + 1)
                                .ok_or(TurnError::Capacity)?,
                            timestamp,
                            SessionFactBody::CancelRequested {
                                turn_id: turn_id.clone(),
                                reason: reason.clone(),
                            },
                        )
                        .map_err(|error| TurnError::Invalid(error.to_string()))?,
                    ));
                }
            }
            facts
        };
        let discarded = if keep_inbox {
            vec![]
        } else {
            inbox.pending_message_ids
        };
        if (!stop_turns || inbox.inbox_paused) && discarded.is_empty() && facts.is_empty() {
            return Ok(CancelResult {
                accepted: false,
                already_terminal: false,
            });
        }
        let parent_append = match header {
            Some(header) => {
                self.stopped_program_input_append(&header, &discarded)
                    .await?
            }
            None => None,
        };
        let mut controls = Vec::with_capacity(discarded.len() + usize::from(stop_turns));
        if stop_turns {
            controls.push(
                AgentControlRecord::new(
                    inbox
                        .durable_control_seq
                        .checked_add(1)
                        .ok_or(TurnError::Capacity)?,
                    timestamp,
                    AgentControlRecordBody::InboxPaused,
                )
                .map_err(|error| TurnError::Invalid(error.to_string()))?,
            );
        }
        for message_id in discarded {
            append_cancelled_message(
                &mut controls,
                inbox.durable_control_seq,
                timestamp,
                message_id,
            )?;
        }
        let mut sessions = vec![AtomicSessionAppend {
            session_id: id.clone(),
            expected_fact_seq: inbox.durable_fact_seq,
            expected_control_seq: inbox.durable_control_seq,
            header: None,
            facts: facts.clone(),
            controls,
        }];
        sessions.extend(parent_append);
        let kernel = self.clone();
        let id = id.clone();
        self.owned_commit(async move {
            let _admission = admission;
            kernel
                .inner
                .commit_agent(AtomicAgentCommit {
                    sessions,
                    required_active_activations: vec![],
                    quiescent_descendants_of: None,
                })
                .await
                .map_err(turn_store_error)?;
            kernel.install_committed_cancellations(&id, inbox.durable_fact_seq, &facts)?;
            kernel.inner.claim_changed.notify_waiters();
            Ok(CancelResult {
                accepted: true,
                already_terminal: false,
            })
        })
        .await
    }

    pub(super) fn install_committed_cancellations(
        &self,
        id: &SessionId,
        expected_seq: u64,
        facts: &[Arc<SessionFact>],
    ) -> TurnResult<()> {
        let Some(last) = facts.last() else {
            return Ok(());
        };
        let mut state = lock_state(&self.inner);
        let session = state.sessions.get_mut(id).ok_or_else(|| {
            TurnError::Invariant("stopped Session disappeared across admitted commit".into())
        })?;
        if session.durable_seq != expected_seq || !session.pending.is_empty() {
            return Err(TurnError::Invariant(
                "stopped Session changed across admitted commit".into(),
            ));
        }
        for fact in facts {
            let SessionFactBody::CancelRequested { turn_id, .. } = fact.body() else {
                unreachable!("cancellation installation owns only CancelRequested Facts");
            };
            let turn = session.turns.get_mut(turn_id).ok_or_else(|| {
                TurnError::Invariant("stopped Turn disappeared across admitted commit".into())
            })?;
            turn.cancel_requested = true;
            turn.cancellation.cancel();
        }
        session.durable_seq = last.seq();
        session.flush_status.send_replace(FlushStatus {
            durable_seq: last.seq(),
            permanent_error: session.permanent_flush_error.clone(),
        });
        publish_live_watermarks(session);
        Ok(())
    }
}

pub(super) fn append_cancelled_message(
    controls: &mut Vec<AgentControlRecord>,
    after: u64,
    timestamp_ms: u64,
    message_id: MessageId,
) -> TurnResult<()> {
    let seq = after
        .checked_add(controls.len() as u64 + 1)
        .ok_or(TurnError::Capacity)?;
    controls.push(
        AgentControlRecord::new(
            seq,
            timestamp_ms,
            AgentControlRecordBody::MessageDiscarded {
                message_id,
                reason: MessageDiscardReason::Cancelled,
            },
        )
        .map_err(|error| TurnError::Invalid(error.to_string()))?,
    );
    Ok(())
}

pub(super) fn append_resume(
    controls: &mut Vec<AgentControlRecord>,
    timestamp_ms: u64,
) -> TurnResult<()> {
    let seq = controls
        .last()
        .expect("resume follows its waking acceptance")
        .seq()
        .checked_add(1)
        .ok_or(TurnError::Capacity)?;
    controls.push(
        AgentControlRecord::new(seq, timestamp_ms, AgentControlRecordBody::InboxResumed)
            .map_err(|error| TurnError::Invalid(error.to_string()))?,
    );
    Ok(())
}
