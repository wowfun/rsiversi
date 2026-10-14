use rsi_agent_session_protocol::ModelAttemptLog;
use rsi_ai_protocol::{ContentDelta, ContentStart, FinishReason, LanguageEvent};

#[test]
fn compact_v1_golden_is_shared_with_the_offline_evidence_reader() {
    let expected = ModelAttemptLog::from_events(
        [
            (
                10,
                LanguageEvent::ContentStarted {
                    index: 0,
                    content: ContentStart::Text,
                },
            ),
            (
                12,
                LanguageEvent::ContentDelta {
                    index: 0,
                    delta: ContentDelta::Text("界🦀".into()),
                },
            ),
            (20, LanguageEvent::ContentFinished { index: 0 }),
            (
                22,
                LanguageEvent::Finished {
                    reason: FinishReason::MaxTokens,
                    replay: None,
                },
            ),
        ],
        true,
    )
    .unwrap();
    let golden: serde_json::Value =
        serde_json::from_str(include_str!("fixtures/model-attempt-v1.json")).unwrap();
    assert_eq!(serde_json::to_value(&expected).unwrap(), golden);
    let decoded: ModelAttemptLog = serde_json::from_value(golden).unwrap();
    assert_eq!(decoded, expected);
}
