//! Bounded validation and summaries must not allocate their full JSON encoding.
#![expect(
    unsafe_code,
    reason = "test allocator delegates unchanged pointers and layouts to System"
)]
use rsi_agent_session_protocol::{
    ActivationId, ActivationOutcome, AgentMessage, AgentMessageContent, AgentMessageSource,
    MessageId, MessageOptions, OutputContract, SessionId,
};
use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
struct Counting;
static ENABLED: AtomicBool = AtomicBool::new(false);
static LARGE: AtomicUsize = AtomicUsize::new(0);
fn observe(size: usize) {
    if size >= 8192 && ENABLED.load(Ordering::Relaxed) {
        LARGE.fetch_add(1, Ordering::Relaxed);
    }
}
// SAFETY: Every operation forwards its original layout and pointer to System;
// allocation-free atomics only observe requested sizes.
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
        // SAFETY: The pointer and layout belong to this delegated allocator.
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
#[test]
fn completion_rejection_and_result_summary_do_not_materialize_full_encoded_values() {
    let message = AgentMessage {
        message_id: MessageId::new("completion").unwrap(),
        source: AgentMessageSource::Completion {
            child_session_id: SessionId::new("child").unwrap(),
            activation_id: ActivationId::new("activation").unwrap(),
            outcome: ActivationOutcome::Completed { result: None },
        },
        content: vec![AgentMessageContent::Text {
            text: "\\".repeat(6000),
        }],
        options: MessageOptions::default(),
    };
    ENABLED.store(true, Ordering::Relaxed);
    let rejected = message.validate();
    ENABLED.store(false, Ordering::Relaxed);
    assert!(
        rejected
            .unwrap_err()
            .to_string()
            .contains("encoded Completion")
    );
    assert_eq!(LARGE.swap(0, Ordering::Relaxed), 0);

    let contract = OutputContract::new(serde_json::json!({"type":"object"})).unwrap();
    contract.validate_value(&serde_json::json!({})).unwrap();
    let value = serde_json::json!({"text":"x".repeat(200_000)});
    ENABLED.store(true, Ordering::Relaxed);
    let summary = contract.summarize(&value);
    ENABLED.store(false, Ordering::Relaxed);
    assert!(summary.is_ok());
    assert_eq!(LARGE.load(Ordering::Relaxed), 0);
}
