use super::{GoalService, Live, Owner, PendingMutation, invalid};
use crate::{GoalDriverStage, GoalError, GoalResult, GoalSnapshot};
use rsi_agent_goal::{
    GOAL_RESERVE, GOAL_SETTLE, GoalPhase, ReserveGoal, RoundSettlement, SettleGoal,
    round_request_id,
};
use rsi_agent_session_protocol::{
    CommandArguments, CommandRevision, ContributionId, SessionCommandInvocation,
};
use rsi_agent_turn_protocol::MessageState;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

impl GoalService {
    pub(super) async fn drive(&self, owner: &Arc<Owner>, live: &Live) -> GoalResult<()> {
        loop {
            let gate = owner.gate.lock().await;
            if live.stop.is_cancelled() {
                return Err(GoalError::ShuttingDown);
            }
            let snapshot = live.session.snapshot().await?;
            let Some(goal) = snapshot
                .state
                .goal
                .as_ref()
                .filter(|goal| goal.id == live.lease.binding().owner)
            else {
                return Ok(());
            };
            if !goal.unsettled() {
                if !live.lease.is_armed() || goal.phase != GoalPhase::Active {
                    return Ok(());
                }
                match self.reserve(owner, live, &snapshot, goal).await {
                    Ok(()) => {}
                    Err(GoalError::Busy) => {
                        owner.publish(live, GoalDriverStage::Waiting, None, None);
                        drop(gate);
                        live.session
                            .wait_idle(&live.lease, live.stop.clone())
                            .await?;
                    }
                    Err(error) => return Err(error),
                }
                continue;
            }
            let reservation = goal
                .reservation
                .as_ref()
                .ok_or_else(|| invalid("unresolved Goal has no reservation"))?;
            let message_id = reservation.message_id.clone();
            if live.session.message_status(&message_id).await?.is_none() {
                return Err(invalid("atomic Goal reservation lacks accepted input"));
            }
            owner.publish(
                live,
                if live.lease.is_armed() {
                    GoalDriverStage::Waiting
                } else {
                    GoalDriverStage::Stopping
                },
                Some(message_id.clone()),
                None,
            );
            drop(gate);
            let outcome = live
                .session
                .wait_round(&message_id, live.stop.clone())
                .await?;
            let _gate = owner.gate.lock().await;
            self.settle(owner, live, &message_id, outcome).await?;
        }
    }

    async fn reserve(
        &self,
        owner: &Owner,
        live: &Live,
        snapshot: &GoalSnapshot,
        goal: &rsi_agent_goal::Goal,
    ) -> GoalResult<()> {
        let mut next = goal.clone();
        next.reserve().map_err(invalid)?;
        let reservation = next
            .reservation
            .as_ref()
            .ok_or_else(|| invalid("Goal reserve returned no input"))?;
        let invocation = SessionCommandInvocation {
            command: ContributionId::new(GOAL_RESERVE).map_err(invalid)?,
            request_id: reservation
                .request_id
                .clone()
                .ok_or_else(|| invalid("durable reserve lacks request identity"))?,
            expected_revision: snapshot.revision,
            arguments: CommandArguments::new(
                serde_json::to_value(ReserveGoal {
                    id: goal.id.clone(),
                })
                .map_err(invalid)?,
            )
            .map_err(invalid)?,
        };
        owner.publish(
            live,
            GoalDriverStage::Reserving,
            Some(reservation.message_id.clone()),
            None,
        );
        live.begin_mutation(
            &invocation,
            &reservation.message_id,
            "reserve",
            matches!(snapshot.revision, CommandRevision::Draft { .. })
                .then(|| reservation.input_sha256.clone()),
        )?;
        let result = if matches!(snapshot.revision, CommandRevision::Draft { .. }) {
            live.session
                .reserve_initial(&live.lease, invocation, reservation.input(&goal.id))
                .await
                .map(|_| ())
        } else {
            live.session
                .internal_command(&live.lease, invocation, Some(reservation.input(&goal.id)))
                .await
                .map(|_| ())
        };
        live.finish_mutation(&result);
        result
    }

    pub(super) async fn settle(
        &self,
        owner: &Owner,
        live: &Live,
        message: &rsi_agent_session_protocol::MessageId,
        outcome: RoundSettlement,
    ) -> GoalResult<()> {
        let snapshot = live.session.snapshot().await?;
        let Some(goal) = snapshot
            .state
            .goal
            .as_ref()
            .filter(|goal| goal.id == live.lease.binding().owner)
        else {
            return Ok(());
        };
        let reservation = goal
            .reservation
            .as_ref()
            .filter(|reservation| &reservation.message_id == message)
            .ok_or(GoalError::Conflict)?;
        if let Some(previous) = &reservation.settlement {
            return if previous == &outcome {
                Ok(())
            } else {
                Err(GoalError::Conflict)
            };
        }
        owner.publish(live, GoalDriverStage::Settling, Some(message.clone()), None);
        let invocation = settlement_invocation(&snapshot, goal, message, outcome)?;
        live.begin_mutation(&invocation, message, "settle", None)?;
        let result = live
            .session
            .internal_command(&live.lease, invocation, None)
            .await
            .map(|_| ());
        live.finish_mutation(&result);
        result
    }

    pub(super) async fn finish_shutdown(&self, owner: &Owner, live: &Live) -> GoalResult<()> {
        let _gate = owner.gate.lock().await;
        let pending = live
            .pending
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        if let Some(pending) = pending {
            let matches = if let Some(digest) = &pending.initial_input {
                let receipt = live.session.message_status(&pending.message).await?;
                let snapshot = live.session.snapshot().await?;
                receipt.is_some_and(|receipt| {
                    receipt.session_id == *live.session.session_id()
                        && receipt.message_id == pending.message
                }) && matches!(snapshot.revision, CommandRevision::Durable { .. })
                    && snapshot.state.goal.as_ref().is_some_and(|goal| {
                        goal.id == live.lease.binding().owner
                            && goal.reservation.as_ref().is_some_and(|reservation| {
                                reservation.message_id == pending.message
                                    && &reservation.input_sha256 == digest
                            })
                    })
            } else {
                let receipt = live
                    .session
                    .internal_status(&live.lease, &pending.request)
                    .await?;
                receipt.as_ref().is_some_and(|receipt| {
                use rsi_agent_session_protocol::DomainMutationSource;
                receipt.session_id() == live.session.session_id()
                    && receipt.commit().request_id() == Some(&pending.request)
                    && matches!(receipt.commit().source(), DomainMutationSource::Continuation { invocation, domain, owner, .. }
                        if domain == &live.lease.binding().domain && owner == &live.lease.binding().owner && invocation.digest().ok().as_deref() == Some(pending.digest.as_str()))
                })
            };
            if !matches {
                return Err(GoalError::OutcomeUnknown(format!(
                    "{} request {} message {}",
                    pending.kind, pending.request, pending.message
                )));
            }
            *live
                .pending
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
        }
        let snapshot = live.session.snapshot().await?;
        let Some(goal) = snapshot
            .state
            .goal
            .as_ref()
            .filter(|goal| goal.id == live.lease.binding().owner)
        else {
            return Ok(());
        };
        let Some(reservation) = goal
            .reservation
            .as_ref()
            .filter(|reservation| reservation.settlement.is_none())
        else {
            return Ok(());
        };
        let Some(receipt) = live
            .session
            .discard_if_pending(&live.lease, &reservation.message_id)
            .await?
        else {
            return Ok(());
        };
        if matches!(receipt.state, MessageState::Claimed { .. }) {
            live.session.cancel(&reservation.message_id).await?;
        }
        let outcome = live
            .session
            .wait_round(&reservation.message_id, CancellationToken::new())
            .await?;
        self.settle(owner, live, &reservation.message_id, outcome)
            .await
    }
}

impl Live {
    fn begin_mutation(
        &self,
        invocation: &SessionCommandInvocation,
        message: &rsi_agent_session_protocol::MessageId,
        kind: &'static str,
        initial_input: Option<String>,
    ) -> GoalResult<()> {
        let mut pending = self
            .pending
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if pending.is_some() {
            return Err(GoalError::OutcomeUnknown(
                "previous Goal mutation remains unresolved".into(),
            ));
        }
        *pending = Some(PendingMutation {
            request: invocation.request_id.clone(),
            message: message.clone(),
            kind,
            digest: invocation.digest().map_err(invalid)?,
            initial_input,
        });
        Ok(())
    }
    fn finish_mutation(&self, result: &GoalResult<()>) {
        if !matches!(
            result,
            Err(GoalError::OutcomeUnknown(_)
                | GoalError::ExecutionOutcomeUnknown
                | GoalError::Backend(_))
        ) {
            *self
                .pending
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
        }
    }
}

fn settlement_invocation(
    snapshot: &GoalSnapshot,
    goal: &rsi_agent_goal::Goal,
    message: &rsi_agent_session_protocol::MessageId,
    outcome: RoundSettlement,
) -> GoalResult<SessionCommandInvocation> {
    let reservation = goal.reservation.as_ref().ok_or(GoalError::Conflict)?;
    Ok(SessionCommandInvocation {
        command: ContributionId::new(GOAL_SETTLE).map_err(invalid)?,
        request_id: round_request_id(&goal.id, reservation.round, "settle").map_err(invalid)?,
        expected_revision: snapshot.revision,
        arguments: CommandArguments::new(
            serde_json::to_value(SettleGoal {
                id: goal.id.clone(),
                message_id: message.clone(),
                settlement: outcome,
            })
            .map_err(invalid)?,
        )
        .map_err(invalid)?,
    })
}
