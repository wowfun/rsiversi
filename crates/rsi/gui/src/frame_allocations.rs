//! Test-thread allocation requests for the changed-pane projection segment.
use std::{
    alloc::{GlobalAlloc, Layout, System},
    cell::Cell,
};
#[derive(Clone, Copy, Debug, Default)]
pub(super) struct Stats {
    pub requested: usize,
    pub calls: usize,
}
thread_local! { static TRACKED: Cell<Option<Stats>> = const { Cell::new(None) }; }
struct Probe;
fn record(bytes: usize) {
    let _ = TRACKED.try_with(|tracked| {
        if let Some(mut stats) = tracked.get() {
            stats.requested += bytes;
            stats.calls += 1;
            tracked.set(Some(stats));
        }
    });
}
// SAFETY: Every operation forwards the unchanged System allocator contract.
// Thread-local counters do not allocate or inspect the returned pointers.
#[allow(unsafe_code)] // Only the unit-test binary observes allocation requests.
unsafe impl GlobalAlloc for Probe {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        record(layout.size());
        // SAFETY: The caller's valid layout is forwarded unchanged.
        unsafe { System.alloc(layout) }
    }
    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        record(layout.size());
        // SAFETY: The caller's valid layout is forwarded unchanged.
        unsafe { System.alloc_zeroed(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: The caller's live allocation and original layout are unchanged.
        unsafe { System.dealloc(ptr, layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        record(size);
        // SAFETY: The caller supplies a live allocation and valid new size.
        unsafe { System.realloc(ptr, layout, size) }
    }
}
#[global_allocator]
static ALLOCATOR: Probe = Probe;
pub(super) fn measure<T>(run: impl FnOnce() -> T) -> (T, Stats) {
    struct Reset;
    impl Drop for Reset {
        fn drop(&mut self) {
            TRACKED.with(|tracked| tracked.set(None));
        }
    }
    let _reset = Reset;
    TRACKED.with(|tracked| tracked.set(Some(Stats::default())));
    let result = run();
    (result, TRACKED.with(|tracked| tracked.take().unwrap()))
}
