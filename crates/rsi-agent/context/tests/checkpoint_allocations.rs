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
fn full_builder_preserves_v8_payload_and_v6_envelope_without_a_third_full_copy() {
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
    // The v8 fold explicitly carries Tool outcome provenance; the generic
    // v6 envelope and Session 17 binding stay unchanged.
    let prefix = b"rsi-agent-model-context-v6\0".len() + 32;
    let metadata_len = u32::from_le_bytes(bytes[prefix..prefix + 4].try_into().unwrap()) as usize;
    let payload = &bytes[prefix + 4 + metadata_len..];
    let fold_magic = b"rsi-agent-context-checkpoint-v8\0";
    assert!(payload.starts_with(fold_magic));
    let decoded: serde_json::Value =
        serde_json::from_slice(&payload[fold_magic.len() + 32..]).unwrap();
    assert_eq!(decoded["version"], 8);
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
        "1bab1152d57d9f24066a5cb853fe9b3f4da7ad1312040a0bc1c5f6d2af2002fe"
    );
    assert!(
        additional < 3 * bytes.len(),
        "full builder retained an avoidable complete checkpoint: {additional}"
    );
    state.restore(&bytes).unwrap();
    assert_planning_allocations(&state, bytes.len());
}

fn assert_planning_allocations(state: &ModelContextState, checkpoint_bytes: usize) {
    let before = LIVE.load(Ordering::Relaxed);
    let allocated_before = ALLOCATED.load(Ordering::Relaxed);
    PEAK.store(before, Ordering::Relaxed);
    let planned = state
        .plan_compaction(
            &rsi_ai_protocol::LanguageRequestOptions::default(),
            &ModelRef::new("fixture", "model").unwrap(),
            &rsi_ai_protocol::LanguageProfile::new(
                128_000,
                4096,
                8192,
                rsi_ai_protocol::ToolDialect::Responses,
                true,
                rsi_ai_protocol::ImageToolResultCapability::No,
                vec![],
            )
            .unwrap(),
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
