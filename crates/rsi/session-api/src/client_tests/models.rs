use super::*;
use rsi_agent_session_protocol::{EffectId, ModelAttemptBuilder, ModelEventPurpose};
use rsi_agent_turn_protocol::{ModelLiveEncoder, ModelLiveSnapshot, ModelLiveStatus};
use rsi_ai_protocol::{ContentDelta, ContentStart, LanguageEvent};

fn frames() -> Vec<Value> {
    let mut builder = ModelAttemptBuilder::new();
    builder
        .push(
            0,
            &LanguageEvent::ContentStarted {
                index: 0,
                content: ContentStart::Text,
            },
        )
        .unwrap();
    let mut encoder = ModelLiveEncoder::default();
    let mut frames = Vec::new();
    for revision in 1..=3 {
        builder
            .push(
                revision,
                &LanguageEvent::ContentDelta {
                    index: 0,
                    delta: ContentDelta::Text("界".repeat(80_000)),
                },
            )
            .unwrap();
        let value = ModelLiveSnapshot {
            session_id: header().session_id().clone(),
            turn_id: TurnId::new("turn").unwrap(),
            effect_id: EffectId::new("model").unwrap(),
            purpose: ModelEventPurpose::Conversation,
            revision,
            prefix: builder.prefix(),
            status: ModelLiveStatus::Streaming,
        };
        frames.extend(
            encoder
                .pages(&value)
                .map(|page| envelope(serde_json::to_value(page).unwrap())),
        );
    }
    frames
}

#[tokio::test]
async fn remote_live_deltas_reconstruct_snapshots_and_keep_decoder_credits_until_drop() {
    let (remote, client, handle) = fixture().await;
    remote.stream(&frames());
    let mut stream = handle.observe_models().await.unwrap();
    let first = stream.next().await.unwrap().unwrap();
    assert_eq!(first.prefix.event_count(), 2);
    let bytes = first.encoded_len();
    assert_eq!(client.state.observations.retained_bytes(), bytes);
    drop(first);
    assert_eq!(
        client.state.observations.retained_bytes(),
        bytes,
        "the decoder still owns its prefix"
    );
    let second = stream.next().await.unwrap().unwrap();
    assert_eq!(second.prefix.event_count(), 3);
    let third = stream.next().await.unwrap().unwrap();
    assert_eq!(third.prefix.event_count(), 4);
    assert_eq!(third.prefix.events().count(), 4);
    assert!(stream.next().await.is_none());
    drop((stream, second, third));
    assert_eq!(client.state.observations.retained_bytes(), 0);
    assert_eq!(remote.output.used(), 0);
}

#[tokio::test]
async fn remote_live_delta_gap_and_partial_eof_fail_closed_and_release_credits() {
    for incomplete in [false, true] {
        let (remote, client, handle) = fixture().await;
        let mut values = frames();
        if incomplete {
            values.pop();
        } else {
            let next = values
                .iter_mut()
                .find(|value| value["body"]["revision"] == 2 && value["body"]["index"] == 0)
                .unwrap();
            next["body"]["offset"] = json!(0);
        }
        remote.stream(&values);
        let mut stream = handle.observe_models().await.unwrap();
        let mut failed = false;
        while let Some(value) = stream.next().await {
            if value.is_err() {
                failed = true;
                break;
            }
        }
        assert!(failed);
        drop(stream);
        assert_eq!(client.state.observations.retained_bytes(), 0);
        assert_eq!(remote.output.used(), 0);
    }
}
