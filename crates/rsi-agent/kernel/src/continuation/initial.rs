//! First automatic input owns a private candidate; an ordinary draft stays uncharged.

use super::*;
use rsi_agent_composition_protocol::{ContributionKind, SessionCommandContext};
use rsi_agent_session_protocol::{CommandRevision, DomainStateView};

impl AgentKernel {
    #[allow(
        clippy::too_many_lines,
        reason = "Keep private first-round construction and its single durable admission together."
    )]
    pub(super) async fn reserve_initial_continuation(
        &self,
        lease: &ContinuationLease,
        prepared: PreparedFreshSession,
        invocation: SessionCommandInvocation,
        input: ContinuationInput,
    ) -> TurnResult<MessageReceipt> {
        self.validate_continuation(lease, prepared.header(), prepared.composition(), true)?;
        self.inner.continuation_issuer.request_round(lease)?;
        input
            .validate()
            .map_err(|error| TurnError::Invalid(error.to_string()))?;
        if input.owner != lease.binding().owner
            || input.round != 1
            || !matches!(invocation.expected_revision, CommandRevision::Draft { .. })
        {
            return Err(TurnError::ContinuationDisarmed);
        }
        let states = prepared.baseline().initial_states();
        let original = states
            .iter()
            .find(|state| state.identity() == &lease.binding().domain)
            .ok_or(TurnError::ContinuationDisarmed)?;
        if original
            .sha256()
            .map_err(|error| TurnError::Invalid(error.to_string()))?
            != lease.binding().snapshot_sha256
        {
            return Err(TurnError::ContinuationDisarmed);
        }
        let callback = prepared
            .composition()
            .contributions()
            .entries()
            .iter()
            .find_map(|entry| match entry.kind() {
                ContributionKind::Command(command)
                    if entry.id() == &invocation.command
                        && command.continuation_kind() == Some(ContinuationCommand::Reserve) =>
                {
                    Some(command.callback().clone())
                }
                _ => None,
            })
            .ok_or_else(|| {
                TurnError::Invalid("initial reservation requires a reserve command".into())
            })?;
        let context = SessionCommandContext {
            request_id: invocation.request_id.clone(),
            continuation_input: Some(input.clone()),
            header: Arc::new(prepared.header().clone()),
            revision: invocation.expected_revision,
            domains: states
                .into_iter()
                .map(|snapshot| DomainStateView {
                    revision: DomainRevision::new(0),
                    snapshot,
                })
                .collect(),
        };
        let cancellation = self.inner.submission_admission.closed.child_token();
        let _cancel = cancellation.clone().drop_guard();
        let proposals = tokio::time::timeout(
            Duration::from_secs(30),
            callback.execute(&context, &invocation.arguments, cancellation),
        )
        .await
        .map_err(|_| TurnError::Invalid("initial reservation exceeded its deadline".into()))?
        .map_err(|error| TurnError::Invalid(bounded_diagnostic(&error.to_string())))?;
        if proposals.len() != 1 || proposals[0].snapshot().identity() != &lease.binding().domain {
            return Err(TurnError::Invalid(
                "initial reservation must replace only its bound domain".into(),
            ));
        }
        let mut baseline = prepared.baseline().clone();
        baseline
            .apply_batch(&proposals)
            .map_err(|error| turn_composition_error(error.into()))?;
        let snapshot_sha256 = proposals[0]
            .snapshot()
            .sha256()
            .map_err(|error| TurnError::Invalid(error.to_string()))?;
        let prepared = prepared
            .with_baseline(baseline)
            .map_err(turn_composition_error)?;
        let admission = self
            .inner
            .submission_admission
            .acquire(prepared.header().session_id())
            .await?;
        self.validate_continuation(lease, prepared.header(), prepared.composition(), true)?;
        if !self.automatic_turn_selected(lease)
            || !self.automatic_session_idle(lease.session_id()).await?
        {
            return Err(TurnError::SessionBusy);
        }
        let source = ContinuationSource {
            domain: lease.binding().domain.clone(),
            owner: input.owner.clone(),
            round: input.round,
            reserved_revision: DomainRevision::new(1),
            provenance: ContinuationProvenance::Baseline { snapshot_sha256 },
            text_sha256: input.text_sha256(),
        };
        self.inner
            .continuation_issuer
            .set_revision(lease, DomainRevision::new(1))?;
        self.inner
            .continuation_issuer
            .guard_source(lease, source.clone())?;
        let receipt = self
            .submit_message_admitted(
                SubmitMessage {
                    session: SubmitSession::Fresh(prepared),
                    delivery: MessageDelivery::NextTurn,
                    message: AgentMessage {
                        message_id: input.message_id,
                        source: AgentMessageSource::Continuation { source },
                        content: vec![AgentMessageContent::Text { text: input.text }],
                        options: MessageOptions::default(),
                    },
                },
                None,
                admission,
            )
            .await?;
        self.inner
            .continuation_issuer
            .admitted_round(lease, receipt.accepted_control_seq)?;
        self.inner.session_changes.committed(lease.session_id());
        Ok(receipt)
    }
}
