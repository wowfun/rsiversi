//! Isolated opt-in allocator observation; run the report test alone.
#![expect(
    unsafe_code,
    reason = "all allocation operations delegate unchanged to System"
)]
use std::{
    alloc::{GlobalAlloc, Layout, System},
    sync::atomic::{AtomicBool, AtomicUsize, Ordering},
};
struct Counting;
static ENABLED: AtomicBool = AtomicBool::new(false);
static CALLS: AtomicUsize = AtomicUsize::new(0);
static BYTES: AtomicUsize = AtomicUsize::new(0);
fn observe(size: usize) {
    if ENABLED.load(Ordering::Relaxed) {
        CALLS.fetch_add(1, Ordering::Relaxed);
        BYTES.fetch_add(size, Ordering::Relaxed);
    }
}
// SAFETY: Original pointer/layout contracts are forwarded unchanged; counters never allocate.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        // SAFETY: Caller supplies a valid allocator layout.
        let result = unsafe { System.alloc(layout) };
        if !result.is_null() {
            observe(layout.size());
        }
        result
    }
    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        // SAFETY: Caller supplies a valid allocator layout.
        let result = unsafe { System.alloc_zeroed(layout) };
        if !result.is_null() {
            observe(layout.size());
        }
        result
    }
    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        // SAFETY: Pointer and original layout belong to this delegated allocator.
        unsafe { System.dealloc(pointer, layout) }
    }
    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        // SAFETY: Pointer, layout and replacement size meet the caller's allocator contract.
        let result = unsafe { System.realloc(pointer, layout, size) };
        if !result.is_null() {
            observe(size);
        }
        result
    }
}
#[global_allocator]
static ALLOCATOR: Counting = Counting;
pub(super) fn begin() {
    CALLS.store(0, Ordering::Relaxed);
    BYTES.store(0, Ordering::Relaxed);
    ENABLED.store(true, Ordering::SeqCst);
}
pub(super) fn end() -> serde_json::Value {
    ENABLED.store(false, Ordering::SeqCst);
    serde_json::json!({"calls":CALLS.load(Ordering::Relaxed),"allocated_bytes":BYTES.load(Ordering::Relaxed)})
}
