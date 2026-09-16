//! The public cold-read and offline-audit seams must reject large indexed text
//! before making a correspondingly large Rust allocation.
#![expect(
    unsafe_code,
    reason = "test allocator delegates every allocation to System and observes sizes only"
)]
use rsi_agent_session_protocol::{
    AgentPresetId, FrozenAgentSettings, SessionFact, SessionFactBody, SessionHeader, SessionId,
    TurnId,
};
use rsi_agent_store_protocol::{AppendBatch, SessionStore, StoreError};
use rsi_agent_store_sqlite::SqliteStore;
use rsi_ai_protocol::ModelRef;
use rsi_sandbox::SandboxMode;
use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

struct Counting;
static ENABLED: AtomicBool = AtomicBool::new(false);
static LARGE: AtomicUsize = AtomicUsize::new(0);
const OVERSIZED: usize = 4 * 1024 * 1024;
fn observe(size: usize) {
    if size >= OVERSIZED && ENABLED.load(Ordering::Relaxed) {
        LARGE.fetch_add(1, Ordering::Relaxed);
    }
}
// SAFETY: All pointers/layouts are forwarded unchanged to System; observation
// uses allocation-free atomics and neither reads nor alters allocated memory.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        observe(layout.size());
        // SAFETY: The caller supplies GlobalAlloc's valid layout.
        unsafe { System.alloc(layout) }
    }
    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        observe(layout.size());
        // SAFETY: The caller supplies GlobalAlloc's valid layout.
        unsafe { System.alloc_zeroed(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: The pointer and original layout belong to this delegated allocator.
        unsafe { System.dealloc(ptr, layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        observe(size);
        // SAFETY: The caller satisfies GlobalAlloc's reallocation contract.
        unsafe { System.realloc(ptr, layout, size) }
    }
}
#[global_allocator]
static ALLOCATOR: Counting = Counting;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn matching_oversized_turn_indexes_fail_before_owned_materialization() {
    let root = tempfile::tempdir().unwrap();
    let store = SqliteStore::open(root.path()).unwrap();
    let header = test_header("oversized");
    let id = header.session_id().clone();
    store
        .append(AppendBatch {
            session_id: id.clone(),
            expected_seq: 0,
            header: Some(header),
            facts: vec![test_fact(1).into()],
        })
        .await
        .unwrap();
    let connection = rusqlite::Connection::open(root.path().join("sessions.sqlite3")).unwrap();
    let corrupt = "x".repeat(OVERSIZED);
    connection
        .execute("UPDATE facts SET turn_id = ?1", [&corrupt])
        .unwrap();
    connection
        .execute("UPDATE turns SET turn_id = ?1", [&corrupt])
        .unwrap();
    drop(corrupt);
    drop(connection);
    ENABLED.store(true, Ordering::SeqCst);
    let inspected = store.inspect_session(&id).await;
    ENABLED.store(false, Ordering::SeqCst);
    let inspected_allocations = LARGE.swap(0, Ordering::SeqCst);
    assert!(matches!(inspected, Err(StoreError::Corrupt(_))));
    assert_eq!(
        inspected_allocations, 0,
        "warm inspection allocated corrupt indexed text"
    );
    drop(store);
    let store = SqliteStore::open(root.path()).unwrap();
    ENABLED.store(true, Ordering::SeqCst);
    let online = store.list_open_turns(&id, 0, 1).await;
    ENABLED.store(false, Ordering::SeqCst);
    let online_allocations = LARGE.swap(0, Ordering::SeqCst);
    assert!(matches!(online, Err(StoreError::Corrupt(_))));
    assert_eq!(
        online_allocations, 0,
        "cold public read allocated corrupt indexed text"
    );
    drop(store);
    ENABLED.store(true, Ordering::SeqCst);
    let offline = SqliteStore::verify(root.path());
    ENABLED.store(false, Ordering::SeqCst);
    let offline_allocations = LARGE.swap(0, Ordering::SeqCst);
    assert!(matches!(offline, Err(StoreError::Corrupt(_))));
    assert_eq!(
        offline_allocations, 0,
        "offline audit allocated corrupt indexed text"
    );
}

fn test_header(session_id: &str) -> SessionHeader {
    SessionHeader::new(
        SessionId::new(session_id).unwrap(),
        1,
        "/workspace",
        AgentPresetId::new("test-agent").unwrap(),
        FrozenAgentSettings::new(
            "default",
            "system",
            ModelRef::new("deployment", "model").unwrap(),
            SandboxMode::WorkspaceWrite,
            false,
        )
        .unwrap(),
    )
    .unwrap()
}

fn test_fact(sequence: u64) -> SessionFact {
    SessionFact::new(
        sequence,
        sequence,
        SessionFactBody::TurnAccepted {
            reasoning_effort: None,
            turn_id: TurnId::new(format!("turn-{sequence}")).unwrap(),
            text: "hello".into(),
            model: None,
            sandbox: SandboxMode::WorkspaceWrite,
            require_approval: false,
        },
    )
    .unwrap()
}
