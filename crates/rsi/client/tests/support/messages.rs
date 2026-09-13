use super::*;
use rsi_agent_session_protocol::{
    ActivationId, AgentControlRecord, AgentControlRecordBody, MessageDiscardReason, StepId,
};
use rsi_agent_turn_protocol::{CancelTarget, ObservationRetention};
use rsi_client::{MessageEvent, MessageRunError, MessageSink, drive_message};
use tokio_util::sync::CancellationToken;

#[derive(Debug)]
pub struct Scenario {
    pub state: MessageState,
    pub message_cancel_accept: bool,
    pub discard: bool,
    pub truncate: bool,
    pub cancellations: Mutex<Vec<CancelTarget>>,
}
impl Default for Scenario {
    fn default() -> Self {
        Self {
            state: MessageState::Pending,
            message_cancel_accept: false,
            discard: false,
            truncate: false,
            cancellations: Mutex::default(),
        }
    }
}
impl Scenario {
    pub fn observations(
        &self,
        cursor: ObservationCursor,
    ) -> Vec<rsi_session_protocol::Result<SessionObservation>> {
        let retention = ObservationRetention::default();
        if cursor.fact_seq == 0 {
            let body = if self.discard {
                AgentControlRecordBody::MessageDiscarded {
                    message_id: MessageId::new("message").unwrap(),
                    reason: MessageDiscardReason::Cancelled,
                }
            } else {
                AgentControlRecordBody::MessageClaimed {
                    message_id: MessageId::new("message").unwrap(),
                    activation_id: ActivationId::new("activation").unwrap(),
                    turn_id: TurnId::new("turn").unwrap(),
                    step_id: StepId::new("step").unwrap(),
                    entered_fact_seq: 1,
                }
            };
            return vec![Ok(SessionObservation::Control {
                record: retention
                    .retain_controls(vec![Arc::new(AgentControlRecord::new(2, 1, body).unwrap())])
                    .unwrap()
                    .pop()
                    .unwrap(),
                durable_control_seq: 100,
            })];
        }
        if self.truncate {
            return Vec::new();
        }
        // A different Turn's terminal Fact cannot finish this message's driver.
        ["another-turn", "turn"]
            .into_iter()
            .enumerate()
            .map(|(index, id)| {
                Ok(SessionObservation::Fact {
                    fact: retention
                        .retain_fact(Arc::new(
                            SessionFact::new(
                                index as u64 + 2,
                                1,
                                SessionFactBody::TurnTerminal {
                                    turn_id: TurnId::new(id).unwrap(),
                                    outcome: TurnOutcome::Completed,
                                },
                            )
                            .unwrap(),
                        ))
                        .unwrap(),
                    durable_fact_seq: 100,
                })
            })
            .collect()
    }
}

#[derive(Debug)]
struct Sink {
    events: Mutex<Vec<MessageEvent>>,
    blocked: bool,
    stopped: bool,
    terminal: Semaphore,
}
impl Default for Sink {
    fn default() -> Self {
        Self {
            events: Mutex::default(),
            blocked: false,
            stopped: false,
            terminal: Semaphore::new(0),
        }
    }
}
#[async_trait]
impl MessageSink for Sink {
    async fn event(&self, event: MessageEvent) -> Result<(), MessageRunError> {
        if self.stopped {
            return Err(MessageRunError::SinkStopped);
        }
        if self.blocked && matches!(event, MessageEvent::Outcome { .. }) {
            self.terminal.acquire().await.unwrap().forget();
        }
        self.events.lock().unwrap().push(event);
        Ok(())
    }
}

fn handle(case: Scenario) -> Arc<Handle> {
    let mut handle = Handle::new("session", false);
    Arc::get_mut(&mut handle).unwrap().message = Some(case);
    handle
}

pub async fn message_claim_cancellation_and_terminal_delivery(execution: Execution) {
    cancellation_race_and_delivery(execution).await;
    resolved_claims_and_failures().await;
}

async fn cancellation_race_and_delivery(execution: Execution) {
    let cancel = CancellationToken::new();
    cancel.cancel();
    let raced = handle(Scenario::default());
    let sink = Arc::new(Sink {
        blocked: true,
        ..Sink::default()
    });
    let task = {
        let raced = raced.clone();
        let sink = sink.clone();
        let cancel = cancel.clone();
        execution.spawn(async move {
            drive_message(raced.as_ref(), input("message"), &cancel, sink.as_ref()).await
        })
    };
    until(&execution, || sink.events.lock().unwrap().len() == 4).await;
    assert_eq!(
        *raced
            .message
            .as_ref()
            .unwrap()
            .cancellations
            .lock()
            .unwrap(),
        [
            CancelTarget::Message(MessageId::new("message").unwrap()),
            CancelTarget::Turn(TurnId::new("turn").unwrap()),
        ]
    );
    assert_eq!(
        raced.active_streams.load(Ordering::SeqCst),
        1,
        "terminal delivery remains owned under backpressure"
    );
    sink.terminal.add_permits(1);
    assert_eq!(task.await.unwrap().unwrap(), TurnOutcome::Completed);
    assert!(
        matches!(sink.events.lock().unwrap().last(), Some(MessageEvent::Outcome { turn_id, .. }) if turn_id.as_str() == "turn")
    );
    assert_eq!(raced.active_streams.load(Ordering::SeqCst), 0);
    assert_eq!(
        *raced.cursors.lock().unwrap(),
        [
            ObservationCursor {
                control_seq: 1,
                fact_seq: 0
            },
            ObservationCursor {
                control_seq: 1,
                fact_seq: 1
            }
        ]
    );

    let accepted = handle(Scenario {
        message_cancel_accept: true,
        ..Scenario::default()
    });
    assert_eq!(
        drive_message(
            accepted.as_ref(),
            input("message"),
            &cancel,
            &Sink::default()
        )
        .await
        .unwrap(),
        TurnOutcome::Cancelled
    );
    assert_eq!(
        accepted
            .message
            .as_ref()
            .unwrap()
            .cancellations
            .lock()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(accepted.active_streams.load(Ordering::SeqCst), 0);
}

async fn resolved_claims_and_failures() {
    let cancel = CancellationToken::new();
    cancel.cancel();
    let claimed = handle(Scenario {
        state: MessageState::Claimed {
            activation_id: ActivationId::new("activation").unwrap(),
            turn_id: TurnId::new("turn").unwrap(),
            step_id: StepId::new("step").unwrap(),
            entered_fact_seq: 1,
        },
        ..Scenario::default()
    });
    drive_message(
        claimed.as_ref(),
        input("message"),
        &cancel,
        &Sink::default(),
    )
    .await
    .unwrap();
    assert_eq!(
        *claimed
            .message
            .as_ref()
            .unwrap()
            .cancellations
            .lock()
            .unwrap(),
        [CancelTarget::Turn(TurnId::new("turn").unwrap())]
    );

    for state in [
        MessageState::Pending,
        MessageState::Discarded {
            reason: MessageDiscardReason::Cancelled,
            control_seq: 2,
        },
    ] {
        let discarded = handle(Scenario {
            state,
            discard: true,
            ..Scenario::default()
        });
        assert!(matches!(
            drive_message(
                discarded.as_ref(),
                input("message"),
                &CancellationToken::new(),
                &Sink::default()
            )
            .await,
            Err(MessageRunError::Discarded { .. })
        ));
        assert_eq!(
            drive_message(
                discarded.as_ref(),
                input("message"),
                &cancel,
                &Sink::default()
            )
            .await
            .unwrap(),
            TurnOutcome::Cancelled
        );
    }
    let truncated = handle(Scenario {
        truncate: true,
        ..Scenario::default()
    });
    assert!(matches!(
        drive_message(
            truncated.as_ref(),
            input("message"),
            &CancellationToken::new(),
            &Sink::default()
        )
        .await,
        Err(MessageRunError::Ended("a terminal Fact"))
    ));
    let stopped = handle(Scenario::default());
    assert!(matches!(
        drive_message(
            stopped.as_ref(),
            input("message"),
            &CancellationToken::new(),
            &Sink {
                stopped: true,
                ..Sink::default()
            }
        )
        .await,
        Err(MessageRunError::SinkStopped)
    ));
    assert!(stopped.cursors.lock().unwrap().is_empty());
}
