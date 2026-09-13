use super::*;
use crate::projection::Transcript;
use rsi_agent_session_protocol::{EffectId, SessionFact, SessionFactBody, TurnId};
use rsi_ai_protocol::{ContentDelta, LanguageEvent};

fn delta(seq: u64, block: u32, text: String) -> SessionFact {
    SessionFact::new(
        seq,
        1,
        SessionFactBody::ModelEvent {
            purpose: rsi_agent_session_protocol::ModelEventPurpose::Conversation,
            turn_id: TurnId::new("turn").unwrap(),
            effect_id: EffectId::new("model").unwrap(),
            event: LanguageEvent::ContentDelta {
                index: block,
                delta: ContentDelta::Text(text),
            },
        },
    )
    .unwrap()
}

#[test]
fn near_limit_transcript_projects_only_changed_blocks_and_counts_exact_wire_bytes() {
    let mut transcript = Transcript::default();
    for index in 0..128 {
        transcript.fact(&delta(u64::from(index) + 1, index, "x".repeat(8000)));
    }
    assert_eq!(transcript.blocks.len(), 128);
    assert_eq!(
        transcript
            .blocks
            .iter()
            .map(|block| block.text.len())
            .sum::<usize>(),
        1_024_000
    );
    let metadata = json!({"transcript":null, "registration":"one"});
    let first = CachedPane::capture(metadata.clone(), Some(&transcript), None).unwrap();
    assert_eq!(first.projected_blocks, 128);
    let mut expected = metadata.clone();
    expected["transcript"] = serde_json::to_value(&transcript).unwrap();
    assert_eq!(serde_json::to_value(&first).unwrap(), expected);
    assert_eq!(first.bytes, serde_json::to_vec(&first).unwrap().len());
    let changed = delta(129, 127, " tail".into());
    transcript.fact(&changed);
    // The comparison covers projection plus size counting, not wire encoding or RSS.
    let baseline_transcript = transcript.clone();
    let ((baseline, _), baseline_allocations) = allocations::measure(|| {
        let value = serde_json::to_value(&baseline_transcript).unwrap();
        let bytes = encoded_size(&value).unwrap();
        (value, bytes)
    });
    let (second, incremental_allocations) = allocations::measure(|| {
        CachedPane::capture(metadata.clone(), Some(&transcript), Some(&first)).unwrap()
    });
    assert_eq!(
        serde_json::to_value(second.transcript.as_ref().unwrap()).unwrap(),
        baseline
    );
    assert!(
        incremental_allocations.requested < baseline_allocations.requested / 10,
        "incremental={incremental_allocations:?}, full={baseline_allocations:?}"
    );
    eprintln!(
        "near-limit changed-pane projection: incremental={incremental_allocations:?}, full={baseline_allocations:?}"
    );
    assert_eq!(second.projected_blocks, 1);
    assert_eq!(second.bytes, serde_json::to_vec(&second).unwrap().len());
    let patch = serde_json::to_value(pane_patch(crate::SurfaceId::MAIN, &first, &second)).unwrap();
    assert_eq!(patch["transcript"]["upsert"].as_array().unwrap().len(), 1);
    assert_eq!(patch["transcript"]["remove"], json!([]));
    assert!(patch["transcript"].get("order").is_none());
    transcript.fact(&changed);
    let duplicate =
        CachedPane::capture(metadata.clone(), Some(&transcript), Some(&second)).unwrap();
    assert_eq!(duplicate.projected_blocks, 0);
    assert_eq!(duplicate, second);
    transcript.status = "Completed".into();
    let mut registration = metadata;
    registration["registration"] = json!("two");
    let third = CachedPane::capture(registration, Some(&transcript), Some(&second)).unwrap();
    assert_eq!(third.projected_blocks, 0);
    assert_eq!(third.bytes, serde_json::to_vec(&third).unwrap().len());
    let patch = serde_json::to_value(pane_patch(crate::SurfaceId::MAIN, &second, &third)).unwrap();
    assert_eq!(patch["fields"], json!({"registration":"two"}));
    assert_eq!(patch["transcript"]["fields"], json!({"status":"Completed"}));
    assert_eq!(patch["transcript"]["upsert"], json!([]));
    // A new generation has no reusable block proof even when its content matches.
    let replaced = CachedPane::capture(third.metadata.clone(), Some(&transcript), None).unwrap();
    assert_eq!(replaced.projected_blocks, 128);
}

#[test]
fn repeated_terminal_facts_reuse_the_closed_status_block() {
    let mut transcript = Transcript::default();
    let fact = SessionFact::new(
        1,
        1,
        SessionFactBody::TurnTerminal {
            turn_id: TurnId::new("turn").unwrap(),
            outcome: rsi_agent_session_protocol::TurnOutcome::Completed,
        },
    )
    .unwrap();
    transcript.fact(&fact);
    let metadata = json!({"transcript":null});
    let first = CachedPane::capture(metadata.clone(), Some(&transcript), None).unwrap();
    transcript.fact(&fact);
    let second = CachedPane::capture(metadata, Some(&transcript), Some(&first)).unwrap();
    assert_eq!(first, second);
    assert_eq!(second.projected_blocks, 0);
}

#[test]
fn repeated_control_preserves_its_history_anchor_after_later_facts() {
    use rsi_agent_session_protocol::{
        AgentControlRecord, AgentControlRecordBody, MessageDiscardReason, MessageId,
    };
    let retention = rsi_agent_turn_protocol::ObservationRetention::default();
    let record = AgentControlRecord::new(
        1,
        1,
        AgentControlRecordBody::MessageDiscarded {
            message_id: MessageId::new("discarded").unwrap(),
            reason: MessageDiscardReason::Cancelled,
        },
    )
    .unwrap();
    let update = rsi_agent_turn_protocol::SessionObservation::Control {
        record: retention
            .retain_controls(vec![Arc::new(record)])
            .unwrap()
            .pop()
            .unwrap(),
        durable_control_seq: 1,
    };
    let mut transcript = Transcript::default();
    transcript.fact(&delta(1, 0, "earlier".into()));
    transcript.observation(&update);
    // Ordinary block admission evicts the older Fact while retaining its following preview.
    for index in 1..128 {
        transcript.fact(&delta(u64::from(index) + 1, index, "later".into()));
    }
    assert!(transcript.omitted);
    assert_eq!(transcript.blocks.front().unwrap().key, "discard:discarded");
    assert_eq!(transcript.history_before(), Some(1));
    let metadata = json!({"transcript":null});
    let first = CachedPane::capture(metadata.clone(), Some(&transcript), None).unwrap();
    transcript.observation(&update);
    assert_eq!(transcript.history_before(), Some(1));
    let duplicate = CachedPane::capture(metadata, Some(&transcript), Some(&first)).unwrap();
    assert_eq!(duplicate.projected_blocks, 0);
    assert_eq!(first, duplicate);
}

#[test]
fn cached_wire_matches_markdown_after_backfill_reordering_and_eviction() {
    let mut transcript = Transcript::default();
    for index in 0..128 {
        transcript.fact(&delta(u64::from(index) + 2, index, "**body**".into()));
    }
    let metadata = json!({"transcript":null});
    let first = CachedPane::capture(metadata.clone(), Some(&transcript), None).unwrap();
    transcript.fact(&delta(1, 127, "*earlier* ".into()));
    let second = CachedPane::capture(metadata.clone(), Some(&transcript), Some(&first)).unwrap();
    assert_eq!(second.projected_blocks, 1);
    let reordered =
        serde_json::to_value(pane_patch(crate::SurfaceId::MAIN, &first, &second)).unwrap();
    assert_eq!(
        reordered["transcript"]["order"][0],
        transcript.blocks[0].key
    );
    assert_eq!(reordered["transcript"]["remove"], json!([]));
    let evicted = transcript.blocks[0].key.clone();
    transcript.fact(&delta(130, 128, "`new`".into()));
    let third = CachedPane::capture(metadata, Some(&transcript), Some(&second)).unwrap();
    assert_eq!(third.projected_blocks, 1);
    let patch = serde_json::to_value(pane_patch(crate::SurfaceId::MAIN, &second, &third)).unwrap();
    assert_eq!(patch["transcript"]["remove"], json!([evicted]));
    assert_eq!(patch["transcript"]["fields"]["omitted"], true);
    assert_eq!(patch["transcript"]["order"].as_array().unwrap().len(), 128);
    assert_eq!(
        serde_json::to_value(&third).unwrap()["transcript"],
        serde_json::to_value(&transcript).unwrap()
    );
    assert_eq!(third.bytes, serde_json::to_vec(&third).unwrap().len());
    let empty = CachedPane::capture(Value::Null, None, Some(&third)).unwrap();
    assert_eq!(serde_json::to_value(&empty).unwrap(), Value::Null);
    assert_eq!(empty.bytes, 4);
}
