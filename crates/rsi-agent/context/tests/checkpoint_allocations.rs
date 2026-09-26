//! Isolated full-builder allocation and exact-byte comparison at the audited base and current tree.
#![expect(
    unsafe_code,
    reason = "isolated allocation regression delegates all memory operations to System"
)]
use rsi_agent_context::{ContextLimits, ContextPage, DefaultContextBuilder, ModelContextState};
use rsi_agent_session_protocol::*;
use rsi_ai_protocol::ModelRef;
use rsi_sandbox::SandboxMode;
use sha2::{Digest, Sha256};
use std::{
    alloc::{GlobalAlloc, Layout, System},
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};
struct Counting;
static LIVE: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);
static ALLOCATED: AtomicUsize = AtomicUsize::new(0);
fn add(size: usize) {
    ALLOCATED.fetch_add(size, Ordering::Relaxed);
    let now = LIVE.fetch_add(size, Ordering::Relaxed) + size;
    PEAK.fetch_max(now, Ordering::Relaxed);
}
// SAFETY: Delegation preserves allocator pointers and layouts; accounting uses atomics only.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        // SAFETY: The caller supplies a valid GlobalAlloc layout.
        let p = unsafe { System.alloc(l) };
        if !p.is_null() {
            add(l.size());
        }
        p
    }
    unsafe fn alloc_zeroed(&self, l: Layout) -> *mut u8 {
        // SAFETY: The caller supplies a valid GlobalAlloc layout.
        let p = unsafe { System.alloc_zeroed(l) };
        if !p.is_null() {
            add(l.size());
        }
        p
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        LIVE.fetch_sub(l.size(), Ordering::Relaxed);
        // SAFETY: This delegated allocator owns the pointer with its original layout.
        unsafe { System.dealloc(p, l) }
    }
    unsafe fn realloc(&self, p: *mut u8, l: Layout, n: usize) -> *mut u8 {
        // SAFETY: The pointer, layout and size satisfy the caller's realloc contract.
        let q = unsafe { System.realloc(p, l, n) };
        if !q.is_null() {
            LIVE.fetch_sub(l.size(), Ordering::Relaxed);
            add(n);
        }
        q
    }
}
#[global_allocator]
static ALLOCATOR: Counting = Counting;
#[test]
fn full_builder_preserves_v10_payload_and_v6_envelope_without_a_third_full_copy() {
    let header = SessionHeader::new(
        SessionId::new("checkpoint-probe").unwrap(),
        1,
        "/workspace",
        AgentPresetId::new("fixture").unwrap(),
        FrozenAgentSettings::new(
            "default",
            "system",
            ModelRef::new("fixture", "model").unwrap(),
            SandboxMode::WorkspaceWrite,
            false,
        )
        .unwrap(),
    )
    .unwrap();
    let mut state = ModelContextState::open(
        Arc::new(DefaultContextBuilder::default()),
        header,
        ContextLimits::new(1024, 32 * 1024 * 1024).unwrap(),
    )
    .unwrap();
    let mut facts = Vec::new();
    for i in 0..24_u64 {
        let turn = TurnId::new(format!("turn-{i}")).unwrap();
        facts.push(
            SessionFact::new(
                i * 2 + 1,
                1,
                SessionFactBody::TurnAccepted {
                    reasoning_effort: None,
                    turn_id: turn.clone(),
                    text: "payload-界\"".repeat(65536),
                    model: None,
                    sandbox: SandboxMode::WorkspaceWrite,
                    require_approval: false,
                },
            )
            .unwrap(),
        );
        facts.push(
            SessionFact::new(
                i * 2 + 2,
                1,
                SessionFactBody::TurnTerminal {
                    turn_id: turn,
                    outcome: TurnOutcome::Completed,
                    result: None,
                },
            )
            .unwrap(),
        );
    }
    let facts: Vec<_> = facts.into_iter().map(Arc::new).collect();
    state.ingest(ContextPage::Canonical(&facts)).unwrap();
    drop(facts);
    let before = LIVE.load(Ordering::Relaxed);
    PEAK.store(before, Ordering::Relaxed);
    let bytes = state.checkpoint().unwrap();
    let additional = PEAK.load(Ordering::Relaxed) - before;
    println!(
        "{{\"checkpoint_bytes\":{},\"peak_additional_bytes\":{},\"sha256\":\"{}\"}}",
        bytes.len(),
        additional,
        hex::encode(Sha256::digest(&bytes))
    );
    // The v10 fold carries complete model Tool identity alongside Program provenance; the generic
    // v6 outer envelope is unchanged; Session 18 changes its header binding.
    let prefix = b"rsi-agent-model-context-v6\0".len() + 32;
    let metadata_len = u32::from_le_bytes(bytes[prefix..prefix + 4].try_into().unwrap()) as usize;
    let payload = &bytes[prefix + 4 + metadata_len..];
    let fold_magic = b"rsi-agent-context-checkpoint-v10\0";
    assert!(payload.starts_with(fold_magic));
    let decoded: serde_json::Value =
        serde_json::from_slice(&payload[fold_magic.len() + 32..]).unwrap();
    assert_eq!(decoded["version"], 10);
    assert_eq!(decoded["turns"].as_array().unwrap().len(), 24);
    assert!(
        decoded["turns"]
            .as_array()
            .unwrap()
            .iter()
            .all(|turn| turn["batches"].as_object().unwrap().is_empty())
    );
    assert_eq!(
        hex::encode(Sha256::digest(&bytes)),
        "4c2c2e6d2ce5f717122a1fd0371547dbe86b3a8fee1a1f197c2efbb32528dd4b"
    );
    assert!(
        additional < 3 * bytes.len(),
        "full builder retained an avoidable complete checkpoint: {additional}"
    );
    state.restore(&bytes).unwrap();
    assert_planning_allocations(&state, bytes.len());
    assert_installation_allocations(&mut state);
}

fn assert_planning_allocations(state: &ModelContextState, checkpoint_bytes: usize) {
    let before = LIVE.load(Ordering::Relaxed);
    let allocated_before = ALLOCATED.load(Ordering::Relaxed);
    PEAK.store(before, Ordering::Relaxed);
    let planned = state
        .plan_compaction(
            &rsi_ai_protocol::LanguageRequestOptions::default(),
            &ModelRef::new("fixture", "model").unwrap(),
            &profile(),
            Some(CompactionTrigger::ProviderContextLimit),
            false,
        )
        .unwrap()
        .unwrap();
    let additional = PEAK.load(Ordering::Relaxed) - before;
    let allocated = ALLOCATED.load(Ordering::Relaxed) - allocated_before;
    println!(
        "planning peak_additional_bytes={additional} allocated_bytes={allocated} selections={}",
        planned.plan.selections.len()
    );
    assert!(
        allocated < 3 * checkpoint_bytes,
        "planning repeatedly copied retained history: {allocated}"
    );
}

fn assert_installation_allocations(state: &mut ModelContextState) {
    use rsi_ai_protocol::{ContentDelta, ContentStart, FinishReason, LanguageEvent};
    let turn = TurnId::new("install-probe").unwrap();
    let effect = EffectId::new("install-summary").unwrap();
    let mut sequence = 49_u64;
    let mut ingest = |state: &mut ModelContextState, body| {
        let fact = Arc::new(SessionFact::new(sequence, 1, body).unwrap());
        sequence += 1;
        state.ingest(ContextPage::Canonical(&[fact])).unwrap();
    };
    ingest(
        state,
        SessionFactBody::TurnAccepted {
            reasoning_effort: None,
            turn_id: turn.clone(),
            text: "current".into(),
            model: None,
            sandbox: SandboxMode::WorkspaceWrite,
            require_approval: false,
        },
    );
    let planned = state
        .plan_compaction(
            &rsi_ai_protocol::LanguageRequestOptions::default(),
            &ModelRef::new("fixture", "model").unwrap(),
            &profile(),
            Some(CompactionTrigger::ProviderContextLimit),
            false,
        )
        .unwrap()
        .unwrap();
    ingest(
        state,
        SessionFactBody::ModelIntent {
            evidence: RequestEvidence::Unavailable {
                reason: EvidenceUnavailable::NotCaptured,
            },
            price_quote: None,
            turn_id: turn.clone(),
            effect_id: effect.clone(),
            purpose: ModelPurpose::ContextCompaction(Box::new(planned.plan)),
            snapshot: summary_snapshot(),
        },
    );
    ingest(
        state,
        SessionFactBody::ModelStarted {
            turn_id: turn.clone(),
            effect_id: effect.clone(),
        },
    );
    for event in [
        LanguageEvent::ContentStarted {
            index: 0,
            content: ContentStart::Text,
        },
        LanguageEvent::ContentDelta {
            index: 0,
            delta: ContentDelta::Text("brief factual summary".into()),
        },
        LanguageEvent::ContentFinished { index: 0 },
    ] {
        ingest(
            state,
            SessionFactBody::ModelEvent {
                purpose: ModelEventPurpose::ContextCompaction,
                turn_id: turn.clone(),
                effect_id: effect.clone(),
                event,
            },
        );
    }
    let allocated_before = ALLOCATED.load(Ordering::Relaxed);
    let before = LIVE.load(Ordering::Relaxed);
    PEAK.store(before, Ordering::Relaxed);
    ingest(
        state,
        SessionFactBody::ModelEvent {
            purpose: ModelEventPurpose::ContextCompaction,
            turn_id: turn,
            effect_id: effect.clone(),
            event: LanguageEvent::Finished {
                reason: FinishReason::Stop,
                replay: None,
            },
        },
    );
    let allocated = ALLOCATED.load(Ordering::Relaxed) - allocated_before;
    let additional = PEAK.load(Ordering::Relaxed).saturating_sub(before);
    assert!(state.summary_installed(&effect));
    println!("installation allocated_bytes={allocated} peak_additional_bytes={additional}");
    // An untouched payload alone is > 700 KiB. Metadata and selected-turn
    // replacements must not clone even one of those retained message bodies.
    assert!(
        allocated < 512 * 1024,
        "installation copied unaffected payloads: {allocated}"
    );
}

fn summary_snapshot() -> rsi_ai_protocol::PreparedCallSnapshot {
    use rsi_ai_protocol::{AiCapability, PreparedCallSnapshot, RetryPolicy};
    PreparedCallSnapshot {
        language_settings: None,
        call_id: "summary".into(),
        deployment_id: "fixture".into(),
        provider_family: "fixture".into(),
        capability: AiCapability::Language,
        model: "model".into(),
        protocol: "test".into(),
        transport: "memory".into(),
        endpoint_fingerprint: "fixture".into(),
        config_generation: 1,
        credential_source: None,
        retry_policy: RetryPolicy::default(),
        request_sha256: "a".repeat(64),
    }
}

fn profile() -> rsi_ai_protocol::LanguageProfile {
    rsi_ai_protocol::LanguageProfile::new(
        128_000,
        4096,
        8192,
        rsi_ai_protocol::ToolDialect::Responses,
        true,
        rsi_ai_protocol::ImageToolResultCapability::No,
        vec![],
    )
    .unwrap()
}
