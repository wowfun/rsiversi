//! Measures the pinned terminal parser's retained heap independently of screen contents.
#![expect(
    unsafe_code,
    reason = "isolated allocation probe delegates pointers and layouts to System"
)]
use std::{
    alloc::{GlobalAlloc, Layout, System},
    sync::atomic::{AtomicUsize, Ordering},
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
fn full_scrollback_alternate_screen_and_shrink_fit_the_high_water_reservation() {
    let line = format!("{}\r\n", "x".repeat(500));
    let before = LIVE.load(Ordering::Relaxed);
    let mut parser = vt100::Parser::new(200, 500, 1000);
    PEAK.store(LIVE.load(Ordering::Relaxed), Ordering::Relaxed);
    for _ in 0..1400 {
        parser.process(line.as_bytes());
    }
    parser.process(b"\x1b[?1049h");
    for _ in 0..200 {
        parser.process(line.as_bytes());
    }
    parser.process(b"\x1b[?1049l");
    // Insertion can retain doubled cell capacity even after truncating a row.
    for row in 1..=200 {
        parser.process(format!("\x1b[{row};1H\x1b[@").as_bytes());
    }
    let full = LIVE.load(Ordering::Relaxed) - before;
    parser.screen_mut().set_size(1, 1);
    let shrunk = LIVE.load(Ordering::Relaxed) - before;
    let peak = PEAK.load(Ordering::Relaxed) - before;
    let high_water_reservation = (200 * 2 + 1000) * 500 * 64 + 64 * 1024;
    assert!(
        peak <= high_water_reservation,
        "peak={peak}, reserved={high_water_reservation}"
    );
    assert!(
        shrunk > (2 + 1000) * 64 + 64 * 1024,
        "shrinking does not free wide scrollback"
    );
    eprintln!(
        "vt100 bytes: full={full}, shrunk={shrunk}, peak={peak}, reserved={high_water_reservation}"
    );
    drop(parser);
}
