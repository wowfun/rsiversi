//! Measures allocations made by the real decoder on one isolated test thread.
use bytes::Bytes;
use futures_util::{StreamExt as _, stream};
use rsi_ai_transport::{DEFAULT_SSE_FRAME_BYTES, SseTermination, decode_sse};
use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

#[derive(Clone, Copy, Debug, Default)]
struct Stats {
    allocations: usize,
    reallocations: usize,
    requested: usize,
    largest: usize,
}
thread_local! {
    static TRACKED: Cell<Option<Stats>> = const { Cell::new(None) };
}
struct TrackingAllocator;
fn record(size: usize, reallocation: bool) {
    let _ = TRACKED.try_with(|tracked| {
        if let Some(mut stats) = tracked.get() {
            stats.allocations += usize::from(!reallocation);
            stats.reallocations += usize::from(reallocation);
            stats.requested += size;
            stats.largest = stats.largest.max(size);
            tracked.set(Some(stats));
        }
    });
}
// SAFETY: All allocation operations forward the unchanged allocator contract to
// System. Thread-local accounting never allocates or dereferences the pointers.
#[allow(unsafe_code)] // This isolated test observes System allocations without changing ownership.
unsafe impl GlobalAlloc for TrackingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        record(layout.size(), false);
        // SAFETY: The caller supplies the GlobalAlloc layout contract.
        unsafe { System.alloc(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: The pointer and original layout are forwarded unchanged.
        unsafe { System.dealloc(ptr, layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        record(size, true);
        // SAFETY: The pointer, original layout and new size satisfy the caller's
        // GlobalAlloc contract and are passed unchanged to the same allocator.
        unsafe { System.realloc(ptr, layout, size) }
    }
}
#[global_allocator]
static ALLOCATOR: TrackingAllocator = TrackingAllocator;

#[tokio::test(flavor = "current_thread")]
async fn small_frames_allocate_in_proportion_to_payload() {
    const EVENTS: usize = 1_000;
    for payload in [32, 1_024] {
        let bytes = Bytes::from(format!("data: {}\n\n", "x".repeat(payload)));
        for width in [bytes.len(), 1] {
            let parts: Vec<_> = (0..bytes.len())
                .step_by(width)
                .map(|start| bytes.slice(start..(start + width).min(bytes.len())))
                .collect();
            let body = stream::iter(0..EVENTS * parts.len())
                .map(move |index| Ok(parts[index % parts.len()].clone()));
            let mut decoded =
                decode_sse(Box::pin(body), SseTermination::Eof, DEFAULT_SSE_FRAME_BYTES);
            TRACKED.set(Some(Stats::default()));
            let mut count = 0;
            while let Some(value) = decoded.next().await {
                assert_eq!(value.unwrap().as_str().len(), payload);
                count += 1;
            }
            drop(decoded);
            let stats = TRACKED.replace(None).unwrap();
            eprintln!("payload={payload} chunk={width} events={count} {stats:?}");
            assert_eq!(count, EVENTS);
            assert!(stats.largest < 64 * 1024, "{stats:?}");
            assert!(stats.requested <= EVENTS * 8 * 1024, "{stats:?}");
        }
    }
}
