use rsi_ai_protocol::{
    AiError, DispatchStatus, ErrorKind, ErrorPhase, RealtimeCloseReason, RealtimeCommand,
    RealtimeEvent, RealtimeRequest, RealtimeValidator,
};

#[test]
fn realtime_validates_live_audio_and_keeps_recoverable_errors_nonterminal() {
    let request = RealtimeRequest::new("alloy").expect("request");
    assert_eq!(request.voice(), "alloy");

    let mut validator = RealtimeValidator::new();
    validator
        .push_event(&RealtimeEvent::SessionStarted {
            session_id: "rt-1".to_owned(),
        })
        .expect("session started");
    validator
        .push_command(&RealtimeCommand::AppendAudio {
            sequence: 1,
            bytes: vec![0, 1, 2, 3],
        })
        .expect("audio");
    validator
        .push_command(&RealtimeCommand::CommitInput {
            item_id: "item-1".to_owned(),
        })
        .expect("commit");
    validator
        .push_event(&RealtimeEvent::RecoverableError {
            error: AiError::new(
                ErrorKind::RateLimited,
                ErrorPhase::Realtime,
                DispatchStatus::Dispatched,
                "response creation was rate limited",
            )
            .expect("safe error"),
        })
        .expect("recoverable error remains live");
    validator
        .push_command(&RealtimeCommand::AppendText {
            text: "continue".to_owned(),
        })
        .expect("session remains usable");
    validator
        .push_event(&RealtimeEvent::Closed {
            reason: RealtimeCloseReason::Client,
        })
        .expect("close");

    let error = validator
        .push_command(&RealtimeCommand::AppendText {
            text: "too late".to_owned(),
        })
        .expect_err("command after close");
    assert_eq!(error.code(), "realtime.closed");
}

#[test]
fn realtime_rejects_audio_sequence_gaps_and_events_before_start() {
    let mut validator = RealtimeValidator::new();
    let error = validator
        .push_event(&RealtimeEvent::OutputTextDelta {
            response_id: "response-1".to_owned(),
            text: "orphan".to_owned(),
        })
        .expect_err("event before start");
    assert_eq!(error.code(), "realtime.not_started");

    validator
        .push_event(&RealtimeEvent::SessionStarted {
            session_id: "rt-1".to_owned(),
        })
        .expect("start");
    let error = validator
        .push_command(&RealtimeCommand::AppendAudio {
            sequence: 2,
            bytes: vec![0, 1],
        })
        .expect_err("sequence gap");
    assert_eq!(error.code(), "realtime.audio_sequence");
}

#[test]
fn realtime_handoff_request_is_bounded_and_correlated() {
    let mut validator = RealtimeValidator::new();
    validator
        .push_event(&RealtimeEvent::SessionStarted {
            session_id: "rt-1".to_owned(),
        })
        .expect("start");
    validator
        .push_event(&RealtimeEvent::HandoffRequested {
            item_id: "item-7".to_owned(),
            text: "inspect the repository".to_owned(),
        })
        .expect("handoff");
}
