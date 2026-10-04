//! The public cold-read and offline-audit seams must reject large indexed text
//! before making a correspondingly large Rust allocation.
#![expect(
    unsafe_code,
    reason = "test allocator delegates every allocation to System and observes sizes only"
)]
use rsi_agent_session_protocol::*;
use rsi_agent_store_protocol::{
    AppendBatch, AtomicAgentCommit, AtomicSessionAppend, SessionStore, StoreError,
};
use rsi_agent_store_sqlite::SqliteStore;
use rsi_ai_protocol::ModelRef;
use rsi_sandbox::SandboxMode;
use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

struct Counting;
static ENABLED: AtomicBool = AtomicBool::new(false);
static LARGE: AtomicUsize = AtomicUsize::new(0);
static LIVE: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);
static ALLOCATED: AtomicUsize = AtomicUsize::new(0);
const OVERSIZED: usize = 4 * 1024 * 1024;
fn observe(size: usize) {
    ALLOCATED.fetch_add(size, Ordering::Relaxed);
    let live = LIVE.fetch_add(size, Ordering::Relaxed) + size;
    PEAK.fetch_max(live, Ordering::Relaxed);
    if size >= OVERSIZED && ENABLED.load(Ordering::Relaxed) {
        LARGE.fetch_add(1, Ordering::Relaxed);
    }
}
// SAFETY: All pointers/layouts are forwarded unchanged to System; observation
// uses allocation-free atomics and neither reads nor alters allocated memory.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        // SAFETY: The caller supplies GlobalAlloc's valid layout.
        let pointer = unsafe { System.alloc(layout) };
        if !pointer.is_null() {
            observe(layout.size());
        }
        pointer
    }
    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        // SAFETY: The caller supplies GlobalAlloc's valid layout.
        let pointer = unsafe { System.alloc_zeroed(layout) };
        if !pointer.is_null() {
            observe(layout.size());
        }
        pointer
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        LIVE.fetch_sub(layout.size(), Ordering::Relaxed);
        // SAFETY: The pointer and original layout belong to this delegated allocator.
        unsafe { System.dealloc(ptr, layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        // SAFETY: The caller satisfies GlobalAlloc's reallocation contract.
        let pointer = unsafe { System.realloc(ptr, layout, size) };
        if !pointer.is_null() {
            LIVE.fetch_sub(layout.size(), Ordering::Relaxed);
            observe(size);
        }
        pointer
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
    excessive_ready_rows_do_not_grow_aggregate_peak().await;
}

fn test_header(session_id: &str) -> SessionHeader {
    SessionHeader::new_local(
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

fn begin_peak() -> (usize, usize) {
    let baseline = LIVE.load(Ordering::Relaxed);
    PEAK.store(baseline, Ordering::Relaxed);
    (baseline, ALLOCATED.load(Ordering::Relaxed))
}
fn end_peak(before: (usize, usize)) -> (usize, usize) {
    (
        PEAK.load(Ordering::Relaxed).saturating_sub(before.0),
        ALLOCATED.load(Ordering::Relaxed) - before.1,
    )
}
#[expect(
    clippy::too_many_lines,
    reason = "fixed canonical history and varying corrupt index rows share one isolated allocator window"
)]
async fn excessive_ready_rows_do_not_grow_aggregate_peak() {
    let root = tempfile::tempdir().unwrap();
    let store = SqliteStore::open(root.path()).unwrap();
    let header = test_header("excess-ready");
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
    let mut controls = vec![];
    for seq in (1..=500).step_by(2) {
        let message_id = MessageId::new(format!("message-{seq}")).unwrap();
        controls.push(
            AgentControlRecord::new(
                seq,
                seq,
                AgentControlRecordBody::MessageAccepted {
                    message: AgentMessage {
                        message_id: message_id.clone(),
                        source: AgentMessageSource::Human,
                        content: vec![AgentMessageContent::Text {
                            text: "bounded".into(),
                        }],
                        options: MessageOptions::default(),
                    },
                    delivery: MessageDelivery::NextTurn,
                    bound_turn_id: None,
                    root_session_id: id.clone(),
                    target: MessageTarget::NextTurn,
                    wake_required: true,
                },
            )
            .unwrap(),
        );
        controls.push(
            AgentControlRecord::new(
                seq + 1,
                seq + 1,
                AgentControlRecordBody::MessageDiscarded {
                    message_id,
                    reason: MessageDiscardReason::Cancelled,
                },
            )
            .unwrap(),
        );
    }
    store
        .commit_agent(AtomicAgentCommit {
            sessions: vec![AtomicSessionAppend {
                session_id: id.clone(),
                expected_fact_seq: 1,
                expected_control_seq: 0,
                header: None,
                facts: vec![],
                controls,
            }],
            required_active_activations: vec![],
            quiescent_descendants_of: None,
        })
        .await
        .unwrap();
    drop(store);
    let mut online = vec![];
    let mut offline = vec![];
    for count in [1, 125, 250] {
        let database = rusqlite::Connection::open(root.path().join("sessions.sqlite3")).unwrap();
        database
            .execute_batch("PRAGMA foreign_keys=ON; DELETE FROM ready_messages")
            .unwrap();
        database.execute("INSERT INTO ready_messages SELECT ?1, ?1, printf('excess-%04d', seq), seq, seq, 'next_turn' FROM agent_controls WHERE session_id=?1 AND seq%2=1 LIMIT ?2", rusqlite::params![id.as_str(), count]).unwrap();
        assert_eq!(
            database
                .query_row("SELECT count(*) FROM pragma_foreign_key_check", [], |row| {
                    row.get::<_, i64>(0)
                })
                .unwrap(),
            0
        );
        drop(database);
        let store = SqliteStore::open(root.path()).unwrap();
        let before = begin_peak();
        assert!(matches!(
            store.read_facts(&id, 0, 1).await,
            Err(StoreError::Corrupt(_))
        ));
        let measured = end_peak(before);
        online.push(measured.0);
        drop(store);
        let before = begin_peak();
        assert!(matches!(
            SqliteStore::verify(root.path()),
            Err(StoreError::Corrupt(_))
        ));
        let audited = end_peak(before);
        offline.push(audited.0);
        println!(
            "ready_rows={count} online_peak={} online_allocated={} offline_peak={} offline_allocated={}",
            measured.0, measured.1, audited.0, audited.1
        );
    }
    for peaks in [online, offline] {
        assert!(
            peaks.iter().max().unwrap() - peaks.iter().min().unwrap() <= 8192,
            "peak Rust retention grew with excessive short index rows: {peaks:?}"
        );
    }
}
