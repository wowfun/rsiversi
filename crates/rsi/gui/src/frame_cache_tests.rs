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
fn queue_frames_reuse_json_until_slots_or_displayed_turn_change() {
    use rsi_agent_session_protocol::{
        AgentMessageSourceKind, MessageDelivery, MessageId, MessageTarget, QueueSlot,
    };
    use rsi_agent_store_protocol::StorePendingMessage;
    let id = MessageId::new("queued").unwrap();
    let entry = StorePendingMessage {
        queue_slot: QueueSlot::initial(&id, 1, 1),
        message_id: id,
        source_kind: AgentMessageSourceKind::Human,
        has_turn_options: false,
        delivery: MessageDelivery::NextTurn,
        target: MessageTarget::NextTurn,
        permits_promotion: false,
        bound_turn_id: None,
        accepted_control_seq: 1,
    };
    let mut live = Transcript::default();
    live.queue.seed(vec![entry.clone()]);
    let metadata = json!({"queue":null,"transcript":null});
    let capture = |live: &Transcript, old| {
        CachedPane::capture_with_queue(metadata.clone(), Some(live), Some(live), old).unwrap()
    };
    let first = capture(&live, None);
    live.fact(&delta(1, 0, "streaming".into()));
    let second = capture(&live, Some(&first));
    assert!(Arc::ptr_eq(
        &first.queue.as_ref().unwrap().content.value,
        &second.queue.as_ref().unwrap().content.value
    ));
    let patch = serde_json::to_value(pane_patch(crate::SurfaceId::MAIN, &first, &second)).unwrap();
    assert!(patch["fields"].get("queue").is_none());
    live.active = Some(TurnId::new("displayed").unwrap());
    let busy = capture(&live, Some(&second));
    let patch = serde_json::to_value(pane_patch(crate::SurfaceId::MAIN, &second, &busy)).unwrap();
    assert_eq!(patch["fields"]["queue"][0]["convert_turn"], "displayed");
    assert_eq!(busy.bytes, serde_json::to_vec(&busy).unwrap().len());
    live.queue.seed(vec![entry.clone()]);
    let unchanged = capture(&live, Some(&busy));
    assert!(Arc::ptr_eq(
        &busy.queue.as_ref().unwrap().content.value,
        &unchanged.queue.as_ref().unwrap().content.value
    ));
    let mut replacement = live.clone();
    replacement.queue = rsi_client::QueueProjection::default();
    replacement.queue.seed(vec![entry]);
    let replaced = capture(&replacement, Some(&busy));
    assert!(!Arc::ptr_eq(
        &busy.queue.as_ref().unwrap().content.value,
        &replaced.queue.as_ref().unwrap().content.value
    ));
    live.queue.seed(vec![]);
    let empty = capture(&live, Some(&busy));
    let patch = serde_json::to_value(pane_patch(crate::SurfaceId::MAIN, &busy, &empty)).unwrap();
    assert_eq!(patch["fields"]["queue"], json!([]));
    assert_eq!(empty.bytes, serde_json::to_vec(&empty).unwrap().len());
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
    assert_eq!(first.projected_turns, 1);
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
    assert_eq!(second.projected_turns, 0);
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
    assert_eq!(third.projected_turns, 0);
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
            result: None,
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

#[test]
fn turn_patches_preserve_revision_and_disjoint_membership() {
    let before = json!({"revision":1,"entries":{"stable":{"answer":[]},"changed":{"answer":[]},"evicted":{"answer":[]}}});
    for after in [
        before.clone(),
        json!({"revision":2,"entries":{"stable":{"answer":[]},"changed":{"answer":["text"]},"new":{"answer":[]}}}),
        json!({"revision":3,"entries":{}}),
        json!({"revision":4,"entries":before["entries"]}),
    ] {
        let patch = serde_json::to_value(turn_patch(&before, &after)).unwrap();
        assert_eq!(patch["revision"], after["revision"]);
        let mut reconstructed = before["entries"].as_object().unwrap().clone();
        for id in patch["remove"].as_array().unwrap() {
            let id = id.as_str().unwrap();
            assert!(patch["upsert"].get(id).is_none());
            reconstructed.remove(id);
        }
        for (id, value) in patch["upsert"].as_object().unwrap() {
            assert_ne!(before["entries"].get(id), Some(value));
            reconstructed.insert(id.clone(), value.clone());
        }
        assert_eq!(Value::Object(reconstructed), after["entries"]);
    }
}

#[test]
fn fresh_index_with_equal_wire_revision_cannot_reuse_another_transcript() {
    let mut one = Transcript::default();
    let mut two = Transcript::default();
    one.fact(&delta(1, 0, "one".into()));
    two.fact(&delta(1, 1, "two".into()));
    assert_eq!(one.turns.view.revision, two.turns.view.revision);
    let metadata = json!({"transcript":null});
    let first = CachedPane::capture(metadata.clone(), Some(&one), None).unwrap();
    let second = CachedPane::capture(metadata, Some(&two), Some(&first)).unwrap();
    assert_eq!(second.projected_turns, 1);
    assert_eq!(
        serde_json::to_value(&second).unwrap()["transcript"],
        serde_json::to_value(&two).unwrap()
    );
}
