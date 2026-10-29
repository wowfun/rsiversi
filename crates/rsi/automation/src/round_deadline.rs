//! Automation's own per-round clock, independent of Agent execution policy.

use futures_util::StreamExt as _;
use rsi_agent_session_protocol::{SessionFact, SessionFactBody, TurnId};
use rsi_agent_turn_protocol::{ObservationCursor, SessionObservation};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::time::Instant;

const ROUND_LIMIT: Duration = Duration::from_mins(2);
const EXPIRED: &str = "Automation round exceeded 120 seconds from acceptance; continuation revoked and controlled cancellation requested";

#[derive(Default)]
struct RoundDeadline(Option<(TurnId, Instant, u64)>);

impl RoundDeadline {
    fn observe(&mut self, fact: &SessionFact, now_ms: u64, now: Instant) -> Result<(), String> {
        match fact.body() {
            SessionFactBody::TurnAccepted { turn_id, .. }
            | SessionFactBody::MessageTurnAccepted { turn_id, .. } => {
                let elapsed = Duration::from_millis(now_ms.saturating_sub(fact.timestamp_ms()));
                self.0 = Some((
                    turn_id.clone(),
                    now + ROUND_LIMIT.saturating_sub(elapsed),
                    fact.timestamp_ms(),
                ));
            }
            SessionFactBody::TurnTerminal { turn_id, .. }
                if self
                    .0
                    .as_ref()
                    .is_some_and(|(current, _, _)| current == turn_id) =>
            {
                if self.0.as_ref().is_some_and(|(_, _, accepted)| {
                    Duration::from_millis(fact.timestamp_ms().saturating_sub(*accepted))
                        > ROUND_LIMIT
                }) {
                    return Err(EXPIRED.into());
                }
                self.0 = None;
            }
            _ => {}
        }
        Ok(())
    }

    async fn expired(&self) {
        match self.0.as_ref() {
            Some((_, deadline, _)) => tokio::time::sleep_until(*deadline).await,
            None => std::future::pending().await,
        }
    }
}

// The caller owns cancellation and awaits its existing Goal cleanup path.
pub(super) async fn monitor(
    handle: &dyn rsi_session_protocol::SessionHandle,
) -> Result<(), String> {
    let horizon = handle
        .history_before(None, 1)
        .await
        .map_err(|error| format!("read round history horizon: {error}"))?
        .durable_seq;
    let facts = handle
        .observe(ObservationCursor::default())
        .await
        .map_err(|error| format!("observe round history: {error}"))?;
    monitor_stream(facts, horizon, || async {
        handle
            .history_before(None, 1)
            .await
            .map(|page| page.durable_seq)
            .map_err(|error| error.to_string())
    })
    .await
}

async fn monitor_stream<F, Fut>(
    mut facts: rsi_session_protocol::SessionObservationStream,
    mut horizon: u64,
    mut durable_horizon: F,
) -> Result<(), String>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<u64, String>>,
{
    let mut round = RoundDeadline::default();
    let mut replaying = horizon != 0;
    let mut seen = 0;
    let mut expiring = None;
    loop {
        tokio::select! {
            biased;
            () = round.expired(), if !replaying => {
                // Reconcile a fixed durable cut: ready traffic cannot keep extending it.
                horizon = durable_horizon().await?;
                if horizon <= seen { return Err(EXPIRED.into()); }
                expiring = round.0.as_ref().map(|(turn, _, _)| turn.clone());
                replaying = true;
            },
            update = facts.next() => {
                match update.ok_or("Automation round observation ended")?.map_err(|error| error.to_string())? {
                    SessionObservation::Fact { fact, .. } => {
                        let now_ms = u64::try_from(SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis()).unwrap_or(u64::MAX);
                        round.observe(&fact, now_ms, Instant::now())?;
                        seen = fact.seq();
                        replaying &= fact.seq() < horizon;
                        if !replaying && let Some(expired) = expiring.take()
                            && round.0.as_ref().is_some_and(|(turn, _, _)| *turn == expired) {
                            return Err(EXPIRED.into());
                        }
                    }
                    SessionObservation::Control { .. } => {}
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(start_paused = true)]
    async fn expiry_drains_one_durable_cut_before_judging_lagged_terminal() {
        use rsi_agent_session_protocol::{EffectId, TurnOutcome};
        use rsi_agent_turn_protocol::ObservationRetention;
        use std::sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        };
        for (terminal_at, expected) in [
            (Some(119_000), "Automation round observation ended"),
            (Some(120_002), EXPIRED),
            (None, EXPIRED),
        ] {
            let turn = TurnId::new("round").unwrap();
            let accepted = SessionFact::new(
                1,
                1,
                SessionFactBody::TurnAccepted {
                    turn_id: turn.clone(),
                    text: "work".into(),
                    model: None,
                    reasoning_effort: None,
                    sandbox: rsi_sandbox::SandboxMode::ReadOnly,
                    require_approval: false,
                },
            )
            .unwrap();
            let body = terminal_at.map_or_else(
                || SessionFactBody::ModelStarted {
                    turn_id: turn.clone(),
                    effect_id: EffectId::new("pending").unwrap(),
                },
                |_| SessionFactBody::TurnTerminal {
                    turn_id: turn.clone(),
                    outcome: TurnOutcome::Completed,
                    result: None,
                },
            );
            let last = SessionFact::new(2, terminal_at.unwrap_or(121_000), body).unwrap();
            let pool = ObservationRetention::default();
            let retained = pool
                .retain_facts(vec![Arc::new(accepted), Arc::new(last)])
                .unwrap();
            let facts = Box::pin(futures_util::stream::iter(retained.into_iter().map(
                |fact| {
                    Ok(SessionObservation::Fact {
                        fact,
                        durable_fact_seq: 2,
                    })
                },
            )));
            let calls = AtomicUsize::new(0);
            let result = monitor_stream(facts, 0, || {
                calls.fetch_add(1, Ordering::Relaxed);
                std::future::ready(Ok(2))
            })
            .await;
            assert_eq!(result.unwrap_err(), expected);
            assert_eq!(calls.load(Ordering::Relaxed), 1);
        }
    }

    #[tokio::test(start_paused = true)]
    async fn acceptance_time_includes_elapsed_history_and_new_round_gets_its_own_clock() {
        let mut round = RoundDeadline::default();
        assert!(futures_util::poll!(std::pin::pin!(round.expired())).is_pending());
        let accepted = |seq, timestamp, id: &str| {
            SessionFact::new(
                seq,
                timestamp,
                SessionFactBody::TurnAccepted {
                    turn_id: TurnId::new(id).unwrap(),
                    text: "work".into(),
                    model: None,
                    reasoning_effort: None,
                    sandbox: rsi_sandbox::SandboxMode::ReadOnly,
                    require_approval: false,
                },
            )
            .unwrap()
        };
        round
            .observe(&accepted(1, 1000, "old"), 120_000, Instant::now())
            .unwrap();
        tokio::time::advance(Duration::from_millis(999)).await;
        assert!(futures_util::poll!(std::pin::pin!(round.expired())).is_pending());
        tokio::time::advance(Duration::from_millis(1)).await;
        assert!(futures_util::poll!(std::pin::pin!(round.expired())).is_ready());
        round
            .observe(&accepted(3, 121_000, "next"), 121_000, Instant::now())
            .unwrap();
        tokio::time::advance(Duration::from_secs(119)).await;
        assert!(futures_util::poll!(std::pin::pin!(round.expired())).is_pending());
        tokio::time::advance(Duration::from_secs(1)).await;
        assert!(futures_util::poll!(std::pin::pin!(round.expired())).is_ready());
        round
            .observe(
                &accepted(5, 1, "expired-at-recovery"),
                999_999,
                Instant::now(),
            )
            .unwrap();
        assert!(futures_util::poll!(std::pin::pin!(round.expired())).is_ready());
    }

    #[tokio::test(start_paused = true)]
    async fn retained_deadline_charges_elapsed_time_and_ends_with_its_exact_turn() {
        let turn = TurnId::new("round").unwrap();
        let mut round = RoundDeadline(Some((turn.clone(), Instant::now() + ROUND_LIMIT, 0)));
        tokio::time::advance(Duration::from_secs(119)).await;
        assert!(futures_util::poll!(std::pin::pin!(round.expired())).is_pending());
        tokio::time::advance(Duration::from_secs(1)).await;
        assert!(futures_util::poll!(std::pin::pin!(round.expired())).is_ready());
        let other = SessionFact::new(
            1,
            1,
            SessionFactBody::TurnTerminal {
                turn_id: TurnId::new("other").unwrap(),
                outcome: rsi_agent_session_protocol::TurnOutcome::Completed,
                result: None,
            },
        )
        .unwrap();
        round.observe(&other, 1, Instant::now()).unwrap();
        assert!(round.0.is_some());
        let terminal = SessionFact::new(
            2,
            2,
            SessionFactBody::TurnTerminal {
                turn_id: turn,
                outcome: rsi_agent_session_protocol::TurnOutcome::Completed,
                result: None,
            },
        )
        .unwrap();
        round.observe(&terminal, 999_999, Instant::now()).unwrap();
        assert!(
            round.0.is_none(),
            "late observation of an on-time terminal must succeed"
        );
        round.0 = Some((TurnId::new("round").unwrap(), Instant::now(), 0));
        let late = SessionFact::new(3, 120_001, terminal.body().clone()).unwrap();
        assert!(round.observe(&late, 120_001, Instant::now()).is_err());
    }
}
