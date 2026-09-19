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
    // Exact v7 fold/v6 envelope with Session 16 and builder 2.6.0.
    // Rebinding only the Header's format and removed workspace-trust field,
    // then both envelope checksums, exactly recovers the Session 15 oracle
    // 33f1e2f8964b04e21337cae1cd789f70a3ee98cbcbbea642bcea2c895e1539b9.
    // All payload bytes and Fact-prefix digests remain unchanged.
    assert_eq!(
        hex::encode(Sha256::digest(&bytes)),
        "092a2720249c31276d7bc3d7a4b56a86cbbb4b4defc4603b225afd7df3c14495"
    );
    assert!(
        additional < 3 * bytes.len(),
        "full builder retained an avoidable complete checkpoint: {additional}"
    );
    state.restore(&bytes).unwrap();
}
