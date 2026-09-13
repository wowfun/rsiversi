use super::{GoalService, Live, Owner, invalid};
use crate::{GoalDriverStage, GoalError, GoalResult, GoalSnapshot};
use rsi_agent_goal::{
    GOAL_RESERVE, GOAL_SETTLE, GoalPhase, ReserveGoal, RoundSettlement, SettleGoal,
    round_request_id,
};
use rsi_agent_session_protocol::{
    CommandArguments, CommandRevision, ContinuationProvenance, ContributionId,
    SessionCommandInvocation,
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
                self.reserve(owner, live, &snapshot, goal).await?;
                continue;
            }
            let reservation = goal
                .reservation
                .as_ref()
                .ok_or_else(|| invalid("unresolved Goal has no reservation"))?;
            let message_id = reservation.message_id.clone();
            if live.session.message_status(&message_id).await?.is_none() {
                if !live.lease.is_armed() || goal.phase != GoalPhase::Active {
                    return Ok(());
                }
                let provenance = match &reservation.request_id {
                    Some(request_id) => ContinuationProvenance::Command {
                        request_id: request_id.clone(),
                    },
                    None if matches!(snapshot.revision, CommandRevision::Draft { .. }) => {
                        ContinuationProvenance::Baseline {
                            snapshot_sha256: snapshot.domain.snapshot.sha256().map_err(invalid)?,
                        }
                    }
                    None => {
                        self.reserve(owner, live, &snapshot, goal).await?;
                        continue;
                    }
                };
                owner.publish(
                    live,
                    GoalDriverStage::Reserving,
                    Some(message_id.clone()),
                    None,
                );
                live.session
                    .submit(&live.lease, reservation.input(&goal.id), provenance)
                    .await?;
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
        live.session
            .internal_command(&live.lease, invocation, Some(reservation.input(&goal.id)))
            .await?;
        Ok(())
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
        live.session
            .internal_command(&live.lease, invocation, None)
            .await?;
        Ok(())
    }

    pub(super) async fn finish_shutdown(&self, owner: &Owner, live: &Live) -> GoalResult<()> {
        let _gate = owner.gate.lock().await;
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
        let outcome = tokio::time::timeout(
            std::time::Duration::from_secs(30),
            live.session
                .wait_round(&reservation.message_id, CancellationToken::new()),
        )
        .await
        .map_err(|_| {
            GoalError::Backend(
                "Goal source Turn did not settle before Host cleanup deadline".into(),
            )
        })??;
        self.settle(owner, live, &reservation.message_id, outcome)
            .await
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
