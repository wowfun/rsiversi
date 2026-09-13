//! Allocation requests on the caller thread of a saturated native executor.
use crate::{LoaderError, worker::NativeExecutor};
use std::{
    alloc::{GlobalAlloc, Layout, System},
    cell::Cell,
    time::Duration,
};

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct Stats {
    calls: usize,
    requested: usize,
}

thread_local! { static TRACKED: Cell<Option<Stats>> = const { Cell::new(None) }; }
struct Probe;

fn record(bytes: usize) {
    let _ = TRACKED.try_with(|tracked| {
        if let Some(mut stats) = tracked.get() {
            stats.calls += 1;
            stats.requested += bytes;
            tracked.set(Some(stats));
        }
    });
}

// SAFETY: All operations forward the System allocator contract unchanged.
// Thread-local counters neither allocate nor inspect allocation contents.
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
        // SAFETY: The caller supplies a live System allocation and its original layout.
        unsafe { System.dealloc(ptr, layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        record(size);
        // SAFETY: The live System allocation, original layout and valid new size are unchanged.
        unsafe { System.realloc(ptr, layout, size) }
    }
}

#[global_allocator]
static ALLOCATOR: Probe = Probe;

fn measure(run: impl FnOnce()) -> Stats {
    struct Reset;
    impl Drop for Reset {
        fn drop(&mut self) {
            TRACKED.with(|tracked| tracked.set(None));
        }
    }
    let _reset = Reset;
    TRACKED.with(|tracked| tracked.set(Some(Stats::default())));
    run();
    TRACKED.with(|tracked| tracked.take().unwrap())
}

fn assert_saturated_rejections_do_not_allocate(blocking: bool) {
    const ATTEMPTS: u64 = 10_000;
    let executor = NativeExecutor::new(1, 1, 1, 1).unwrap();
    let (started, entered) = std::sync::mpsc::sync_channel(1);
    let (release, blocked) = std::sync::mpsc::sync_channel(1);
    let first = executor
        .spawn_blocking_callback("held", move || {
            started.send(()).unwrap();
            let _ = blocked.recv();
        })
        .unwrap();
    entered.recv_timeout(Duration::from_secs(5)).unwrap();
    let stats = measure(|| {
        for _ in 0..ATTEMPTS {
            let rejected = if blocking {
                executor.spawn_blocking_callback("rejected", || ()).err()
            } else {
                executor.spawn_callback("rejected", || ()).err()
            };
            assert!(matches!(
                rejected,
                Some(LoaderError::Busy {
                    operation: "rejected"
                })
            ));
        }
    });
    let snapshot = executor.snapshot();
    release.send(()).unwrap();
    first.recv_timeout(Duration::from_secs(5)).unwrap();
    eprintln!("blocking={blocking}: {stats:?} for {ATTEMPTS} rejections");
    assert_eq!(stats, Stats::default());
    assert_eq!(snapshot.active_callbacks, 1);
    assert_eq!(snapshot.peak_callbacks, 1);
    assert_eq!(snapshot.callback_thread_starts, 1);
    assert_eq!(snapshot.rejected_callbacks, ATTEMPTS);
}

#[test]
fn saturated_async_callbacks_reject_without_allocating() {
    assert_saturated_rejections_do_not_allocate(false);
}

#[test]
fn saturated_blocking_callbacks_reject_without_allocating() {
    assert_saturated_rejections_do_not_allocate(true);
}
