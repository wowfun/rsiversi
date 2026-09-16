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
fn add(size: usize) {
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
fn full_builder_preserves_v7_payload_and_v6_envelope_without_a_third_full_copy() {
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
    // Exact v7 fold inside its v6 envelope, with Session 14 and builder 2.5.0.
    // Rebinding only those two versions to 13 / 2.4.0 reproduces the audited
    // f3c7a352 oracle d17d4323502379278d4936ea1203b3a15110242a3494d46b72e7df95efd677cb.
    assert_eq!(
        hex::encode(Sha256::digest(&bytes)),
        "b2527575decf8a97f49976f0bd7e38a35b26331bbcf45cb530596122dac77702"
    );
    assert!(
        additional < 3 * bytes.len(),
        "full builder retained an avoidable complete checkpoint: {additional}"
    );
    state.restore(&bytes).unwrap();
}
