//! Generation-pinned effect-free commands with canonical receipt reconciliation.

use super::*;
use rsi_agent_composition_protocol::{
    ContributionKind, SessionCommandContext, SessionCommandRegistration, ValidatedDomainProposal,
};
use rsi_agent_session_protocol::SessionCommandsView;
use rsi_agent_session_protocol::{
    CommandRevision, DomainMutationSource, DomainRequestId, DomainStateCommit, DomainStateUpdate,
    DomainStateView, SessionCommandInvocation,
};
use rsi_agent_turn_protocol::{DomainMutationReceipt, SessionCommands};

type CommandKey = (SessionId, DomainRequestId);
type CommandResult = TurnResult<DomainMutationReceipt>;

#[derive(Default)]
pub(super) struct CommandRequests(Mutex<BTreeMap<CommandKey, PendingCommand>>);

struct PendingCommand {
    digest: String,
    receiver: watch::Receiver<Option<CommandResult>>,
}

struct CommandGuard {
    kernel: AgentKernel,
    key: CommandKey,
    sender: watch::Sender<Option<CommandResult>>,
}

impl Drop for CommandGuard {
    fn drop(&mut self) {
        if self.sender.borrow().is_none() {
            self.sender.send_replace(Some(Err(TurnError::Invariant(
                "command task ended without a result".into(),
            ))));
        }
        self.kernel
            .inner
            .commands
            .0
            .lock()
            .expect("command requests poisoned")
            .remove(&self.key);
    }
}

#[async_trait]
impl SessionCommands for AgentKernel {
    async fn list(&self, session: PreparedResumeSession) -> TurnResult<SessionCommandsView> {
        let (header, composition) = self.inner.resume_issuer.inspect(&session)?;
        let page = observation::read_domain_states_bounded(&self.inner, header.session_id(), None)
            .await
            .map_err(turn_store_error)?;
        SessionCommandsView::new(
            CommandRevision::Durable {
                control_seq: page.durable_control_seq,
            },
            composition
                .contributions()
                .entries()
                .iter()
                .filter_map(|entry| match entry.kind() {
                    ContributionKind::Command(command) => Some(command.descriptor().clone()),
                    _ => None,
                })
                .collect(),
        )
        .map_err(|error| TurnError::Invariant(error.to_string()))
    }

    async fn execute(
        &self,
        session: PreparedResumeSession,
        invocation: SessionCommandInvocation,
    ) -> CommandResult {
        let (header, _) = self.inner.resume_issuer.inspect(&session)?;
        let key = (header.session_id().clone(), invocation.request_id.clone());
        let digest = invocation
            .digest()
            .map_err(|error| TurnError::Invalid(error.to_string()))?;
        let mut receiver = {
            // This is admission only; no user callback or Store I/O runs under either lock.
            let state = lock_state(&self.inner);
            if !state.accepting {
                return Err(TurnError::ShuttingDown);
            }
            let mut pending = self
                .inner
                .commands
                .0
                .lock()
                .expect("command requests poisoned");
            if let Some(existing) = pending.get(&key) {
                if existing.digest != digest {
                    return Err(request_conflict(&invocation));
                }
                existing.receiver.clone()
            } else {
                if pending.len() >= 64 {
                    return Err(TurnError::Capacity);
                }
                let (sender, receiver) = watch::channel(None);
                pending.insert(
                    key.clone(),
                    PendingCommand {
                        digest,
                        receiver: receiver.clone(),
                    },
                );
                let guard = CommandGuard {
                    kernel: self.clone(),
                    key,
                    sender,
                };
                let kernel = self.clone();
                self.inner.tasks.spawn(async move {
                    let result = std::panic::AssertUnwindSafe(
                        kernel.execute_session_command(session, invocation),
                    )
                    .catch_unwind()
                    .await
                    .unwrap_or_else(|_| {
                        Err(TurnError::Invariant("command callback panicked".into()))
                    });
                    guard.sender.send_replace(Some(result));
                    drop(guard);
                });
                receiver
            }
        };
        loop {
            if let Some(result) = receiver.borrow_and_update().clone() {
                return result;
            }
            receiver
                .changed()
                .await
                .map_err(|_| TurnError::Invariant("command result owner disappeared".into()))?;
        }
    }

    async fn query(
        &self,
        session_id: &SessionId,
        request_id: &DomainRequestId,
    ) -> TurnResult<Option<DomainMutationReceipt>> {
        let result = self.domain_request(session_id, request_id).await?;
        if result.as_ref().is_some_and(|receipt| {
            !matches!(
                receipt.commit().source(),
                DomainMutationSource::Command { .. }
            )
        }) {
            return Err(TurnError::DomainRequestConflict {
                request_id: request_id.to_string(),
            });
        }
        Ok(result)
    }
}

struct CommandCandidate {
    session: PreparedResumeSession,
    invocation: SessionCommandInvocation,
    proposals: Vec<ValidatedDomainProposal>,
}

struct PreparedCommandCommit {
    candidate: CommandCandidate,
    _admission: SubmissionAdmissionLease,
    append: AtomicSessionAppend,
    receipt: DomainMutationReceipt,
}

impl AgentKernel {
    async fn execute_session_command(
        &self,
        session: PreparedResumeSession,
        invocation: SessionCommandInvocation,
    ) -> CommandResult {
        let cancellation = self.inner.submission_admission.closed.child_token();
        let _cancel = cancellation.clone().drop_guard();
        let deadline = Instant::now() + Duration::from_secs(30);
        let candidate = tokio::select! {
            biased;
            () = cancellation.cancelled() => return Err(TurnError::ShuttingDown),
            result = tokio::time::timeout_at(deadline, self.prepare_session_command(session, invocation, cancellation.clone())) => {
                result.map_err(|_| TurnError::Invalid("command preparation exceeded its deadline".into()))??
            }
        };
        match candidate {
            Ok(receipt) => Ok(receipt),
            Err(candidate) => self.commit_session_command(candidate).await,
        }
    }

    async fn prepare_session_command(
        &self,
        session: PreparedResumeSession,
        invocation: SessionCommandInvocation,
        cancellation: CancellationToken,
    ) -> TurnResult<std::result::Result<DomainMutationReceipt, PreparedCommandCommit>> {
        let (header, composition) = self.inner.resume_issuer.inspect(&session)?;
        if let Some(receipt) = self
            .domain_request(header.session_id(), &invocation.request_id)
            .await?
        {
            return matching_receipt(receipt, &invocation).map(Ok);
        }
        let callback: SessionCommandRegistration = composition
            .contributions()
            .entries()
            .iter()
            .find_map(|entry| match entry.kind() {
                ContributionKind::Command(command) if entry.id() == &invocation.command => {
                    Some(command.clone())
                }
                _ => None,
            })
            .ok_or_else(|| {
                TurnError::Invalid(format!(
                    "command {} is unavailable in this generation",
                    invocation.command
                ))
            })?;
        let page = observation::read_domain_states_bounded(&self.inner, header.session_id(), None)
            .await
            .map_err(turn_store_error)?;
        check_revision(&invocation, page.durable_control_seq)?;
        composition
            .domains()
            .validate_complete_states(
                &page
                    .states
                    .iter()
                    .map(|state| state.snapshot.clone())
                    .collect::<Vec<_>>(),
            )
            .map_err(|error| turn_composition_error(error.into()))?;
        let context = SessionCommandContext {
            header: Arc::new(header.clone()),
            revision: invocation.expected_revision,
            domains: page
                .states
                .into_iter()
                .map(|state| DomainStateView {
                    revision: state.head.revision,
                    snapshot: state.snapshot,
                })
                .collect(),
        };
        let proposals = callback
            .callback()
            .execute(&context, &invocation.arguments, cancellation)
            .await
            .map_err(|error| {
                TurnError::Invalid(format!(
                    "command {}: {}",
                    invocation.command,
                    bounded_diagnostic(&error.to_string())
                ))
            })?;
        self.stage_session_command(CommandCandidate {
            session,
            invocation,
            proposals,
        })
        .await
    }

    async fn stage_session_command(
        &self,
        mut candidate: CommandCandidate,
    ) -> TurnResult<std::result::Result<DomainMutationReceipt, PreparedCommandCommit>> {
        if candidate.proposals.is_empty()
            || candidate.proposals.len() > rsi_agent_session_protocol::MAXIMUM_SESSION_DOMAINS
        {
            return Err(TurnError::Invalid(
                "command requires 1..=64 typed domain replacements".into(),
            ));
        }
        let (header, composition) = self.inner.resume_issuer.inspect(&candidate.session)?;
        let session_id = header.session_id().clone();
        let admission = self.inner.submission_admission.acquire(&session_id).await?;
        self.fence_pending_terminal(&session_id).await?;
        if let Some(receipt) = self
            .domain_request(&session_id, &candidate.invocation.request_id)
            .await?
        {
            return matching_receipt(receipt, &candidate.invocation).map(Ok);
        }
        let page = observation::read_domain_states_bounded(&self.inner, &session_id, None)
            .await
            .map_err(turn_store_error)?;
        check_revision(&candidate.invocation, page.durable_control_seq)?;
        candidate.proposals.sort_by(|a, b| {
            a.snapshot()
                .identity()
                .id()
                .cmp(b.snapshot().identity().id())
        });
        let updates = candidate
            .proposals
            .iter()
            .map(|proposal| {
                composition
                    .domains()
                    .validate_proposal(proposal)
                    .map_err(|error| turn_composition_error(error.into()))?;
                DomainStateUpdate::new(proposal.expected_revision(), proposal.snapshot().clone())
                    .map_err(|error| TurnError::Invalid(error.to_string()))
            })
            .collect::<TurnResult<Vec<_>>>()?;
        let commit = DomainStateCommit::new(
            Some(candidate.invocation.request_id.clone()),
            DomainMutationSource::Command {
                invocation: candidate.invocation.clone(),
            },
            updates,
        )
        .map_err(|error| TurnError::Invalid(error.to_string()))?;
        let seq = page
            .durable_control_seq
            .checked_add(1)
            .ok_or_else(|| TurnError::Invariant("control sequence exhausted".into()))?;
        rsi_agent_store_protocol::domain_heads_after(
            &page
                .states
                .iter()
                .map(|state| state.head.clone())
                .collect::<Vec<_>>(),
            seq,
            &commit,
        )
        .map_err(turn_store_error)?;
        let control = AgentControlRecord::new(
            seq,
            self.inner.clock.now_ms().max(1),
            AgentControlRecordBody::DomainStateCommitted { commit },
        )
        .map_err(|error| TurnError::Invalid(error.to_string()))?;
        let receipt = DomainMutationReceipt::new(session_id.clone(), control.clone())?;
        Ok(Err(PreparedCommandCommit {
            candidate,
            _admission: admission,
            append: AtomicSessionAppend {
                session_id,
                expected_fact_seq: page.durable_fact_seq,
                expected_control_seq: page.durable_control_seq,
                header: None,
                facts: vec![],
                controls: vec![control],
            },
            receipt,
        }))
    }

    async fn commit_session_command(&self, prepared: PreparedCommandCommit) -> CommandResult {
        let PreparedCommandCommit {
            candidate,
            _admission,
            append,
            receipt,
        } = prepared;
        let session_id = append.session_id.clone();
        let result = self
            .commit_agent_with_flush_conflict_retry(AtomicAgentCommit {
                sessions: vec![append],
                required_active_activations: vec![],
                quiescent_descendants_of: None,
            })
            .await;
        if let Err(error) = result.and_then(|result| result.map_err(turn_store_error)) {
            match self
                .domain_request(&session_id, &candidate.invocation.request_id)
                .await
            {
                Ok(Some(stored)) => {
                    if stored != receipt {
                        return Err(request_conflict(&candidate.invocation));
                    }
                    self.inner.session_changes.committed(&session_id);
                    return Ok(stored);
                }
                Ok(None) => return Err(error),
                Err(_) => {
                    return Err(TurnError::DomainOutcomeUnknown {
                        request_id: candidate.invocation.request_id.to_string(),
                    });
                }
            }
        }
        self.inner.session_changes.committed(&session_id);
        Ok(receipt)
    }
}

fn request_conflict(invocation: &SessionCommandInvocation) -> TurnError {
    TurnError::DomainRequestConflict {
        request_id: invocation.request_id.to_string(),
    }
}

fn matching_receipt(
    receipt: DomainMutationReceipt,
    invocation: &SessionCommandInvocation,
) -> CommandResult {
    let DomainMutationSource::Command { invocation: stored } = receipt.commit().source() else {
        return Err(request_conflict(invocation));
    };
    let digest = |input: &SessionCommandInvocation| {
        input
            .digest()
            .map_err(|error| TurnError::Invalid(error.to_string()))
    };
    if digest(stored)? != digest(invocation)? {
        return Err(request_conflict(invocation));
    }
    Ok(receipt)
}

fn check_revision(invocation: &SessionCommandInvocation, control_seq: u64) -> TurnResult<()> {
    let actual = CommandRevision::Durable { control_seq };
    if invocation.expected_revision != actual {
        return Err(TurnError::CommandRevisionConflict {
            expected: invocation.expected_revision,
            actual,
        });
    }
    Ok(())
}
