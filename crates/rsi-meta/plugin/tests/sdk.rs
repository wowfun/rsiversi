#![allow(unsafe_code)] // Exercises the audited raw ABI boundary directly.

use core::ffi::c_void;
use rsi_meta_plugin::sdk::{Host, Plugin};
use rsi_meta_plugin::{
    CALL_OK, CALL_PANICKED, HostApi, INIT_OK, Lane, POST_FRAME_ACCEPTED, POST_FRAME_CLOSED,
    POST_FRAME_WOULD_BLOCK, PluginApi, PostFrameOutcome,
};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

#[derive(Debug)]
struct HostState {
    frames: Mutex<Vec<(u32, Vec<u8>)>>,
}

unsafe extern "C" fn host_post_frame(
    handle: *mut c_void,
    lane: u32,
    data_ptr: *const u8,
    data_len: usize,
) -> u32 {
    // SAFETY: Each test passes an `Arc<HostState>` raw pointer that remains alive
    // until after the plugin is destroyed.
    let state = unsafe { &*handle.cast::<HostState>() };
    let bytes = if data_len == 0 {
        &[]
    } else {
        // SAFETY: The ABI guarantees a readable buffer for this call.
        unsafe { std::slice::from_raw_parts(data_ptr, data_len) }
    };
    state.frames.lock().unwrap().push((lane, bytes.to_vec()));
    match bytes.first() {
        Some(1) => POST_FRAME_WOULD_BLOCK,
        Some(2) => POST_FRAME_CLOSED,
        _ => POST_FRAME_ACCEPTED,
    }
}

#[derive(Debug)]
struct EchoPlugin {
    host: Host,
}

impl Plugin for EchoPlugin {
    type Error = &'static str;

    fn create(host: Host) -> Result<Self, Self::Error> {
        Ok(Self { host })
    }

    fn on_frame(&mut self, lane: Lane, payload: &[u8]) -> Result<(), Self::Error> {
        assert!(payload != b"panic", "caught by the SDK trampoline");
        let outcome = self.host.post_frame(lane, payload).map_err(|_| "host")?;
        match outcome {
            PostFrameOutcome::Accepted
            | PostFrameOutcome::WouldBlock
            | PostFrameOutcome::Closed => Ok(()),
            PostFrameOutcome::Unknown(_) => Err("unknown post status"),
        }
    }
}

rsi_meta_plugin::export_plugin!(EchoPlugin);

#[test]
fn safe_sdk_preserves_lanes_and_backpressure_statuses() {
    let state = Arc::new(HostState {
        frames: Mutex::new(Vec::new()),
    });
    // Keep this Arc alive until after destroy; the Host API borrows its pointee.
    let handle = Arc::as_ptr(&state).cast_mut().cast::<c_void>();
    // SAFETY: `state` outlives the plugin and the callback obeys the ABI.
    let host = unsafe { HostApi::new(handle, host_post_frame) };
    let mut plugin = PluginApi::EMPTY;

    // SAFETY: Both pointers reference valid tables for the duration of the call.
    let init = unsafe {
        rsi_meta_plugin_entry_v0(
            &raw const host,
            &raw mut plugin,
            core::mem::size_of::<PluginApi>(),
        )
    };
    assert_eq!(init, INIT_OK);
    assert!(plugin.is_compatible());

    let on_frame = plugin.on_frame.unwrap();
    for (lane, payload) in [
        (Lane::Control, b"control".as_slice()),
        (Lane::Data, &[1]),
        (Lane::Data, &[2]),
    ] {
        // SAFETY: The plugin handle is live and the slice is valid for this call.
        let status = unsafe {
            on_frame(
                plugin.plugin_handle,
                lane.as_raw(),
                payload.as_ptr(),
                payload.len(),
            )
        };
        assert_eq!(status, CALL_OK);
    }

    let frames = state.frames.lock().unwrap();
    assert_eq!(frames[0], (Lane::Control.as_raw(), b"control".to_vec()));
    assert_eq!(frames[1], (Lane::Data.as_raw(), vec![1]));
    assert_eq!(frames[2], (Lane::Data.as_raw(), vec![2]));
    drop(frames);

    assert_eq!(
        // SAFETY: The ABI allows one destroy call for this live handle.
        unsafe { plugin.destroy.unwrap()(plugin.plugin_handle) },
        CALL_OK
    );
}

#[test]
fn plugin_panic_is_caught_before_the_ffi_boundary() {
    let state = Arc::new(HostState {
        frames: Mutex::new(Vec::new()),
    });
    let handle = Arc::as_ptr(&state).cast_mut().cast::<c_void>();
    // SAFETY: `state` and callback meet `HostApi::new`'s lifetime contract.
    let host = unsafe { HostApi::new(handle, host_post_frame) };
    let mut plugin = PluginApi::EMPTY;
    assert_eq!(
        // SAFETY: Valid input/output table pointers.
        unsafe {
            rsi_meta_plugin_entry_v0(
                &raw const host,
                &raw mut plugin,
                core::mem::size_of::<PluginApi>(),
            )
        },
        INIT_OK
    );

    let payload = b"panic";
    // SAFETY: Live handle and readable payload.
    let status = unsafe {
        plugin.on_frame.unwrap()(
            plugin.plugin_handle,
            Lane::Data.as_raw(),
            payload.as_ptr(),
            payload.len(),
        )
    };
    assert_eq!(status, CALL_PANICKED);

    // The poisoned state is still safely reclaimed; destroy itself cannot unwind.
    // SAFETY: This is the single destroy call for the handle.
    let _ = unsafe { plugin.destroy.unwrap()(plugin.plugin_handle) };
}

#[test]
fn plugin_entry_rejects_output_storage_smaller_than_the_current_table() {
    let state = Arc::new(HostState {
        frames: Mutex::new(Vec::new()),
    });
    let handle = Arc::as_ptr(&state).cast_mut().cast::<c_void>();
    // SAFETY: `state` and callback meet `HostApi::new`'s lifetime contract.
    let host = unsafe { HostApi::new(handle, host_post_frame) };
    let mut plugin = PluginApi::EMPTY;

    assert_eq!(
        // SAFETY: The output pointer is valid, but the advertised capacity is
        // deliberately one byte short and therefore must not be written.
        unsafe {
            rsi_meta_plugin_entry_v0(
                &raw const host,
                &raw mut plugin,
                PluginApi::STRUCT_SIZE as usize - 1,
            )
        },
        rsi_meta_plugin::INIT_INVALID_HOST_API
    );
    assert_eq!(
        plugin.struct_size, 0,
        "undersized output must stay untouched"
    );
}

#[test]
fn host_table_rejects_null_context_and_nonzero_reserved_word() {
    // SAFETY: The table is inspected only; the callback is never invoked.
    let null_host = unsafe { HostApi::new(core::ptr::null_mut(), host_post_frame) };
    assert!(!null_host.is_compatible());

    let state = Arc::new(HostState {
        frames: Mutex::new(Vec::new()),
    });
    let handle = Arc::as_ptr(&state).cast_mut().cast::<c_void>();
    // SAFETY: `state` outlives this table.
    let mut reserved = unsafe { HostApi::new(handle, host_post_frame) };
    reserved.reserved = 1;
    assert!(!reserved.is_compatible());
}

static SHUTDOWN_ATTEMPTS: AtomicU32 = AtomicU32::new(0);

struct FailOnceShutdownPlugin;

impl Plugin for FailOnceShutdownPlugin {
    type Error = &'static str;

    fn create(_host: Host) -> Result<Self, Self::Error> {
        SHUTDOWN_ATTEMPTS.store(0, Ordering::SeqCst);
        Ok(Self)
    }

    fn on_frame(&mut self, _lane: Lane, _payload: &[u8]) -> Result<(), Self::Error> {
        Ok(())
    }

    fn shutdown(&mut self) -> Result<(), Self::Error> {
        if SHUTDOWN_ATTEMPTS.fetch_add(1, Ordering::SeqCst) == 0 {
            Err("retry shutdown")
        } else {
            Ok(())
        }
    }
}

#[test]
fn failed_shutdown_can_be_retried_before_destroy() {
    let state = Arc::new(HostState {
        frames: Mutex::new(Vec::new()),
    });
    let handle = Arc::as_ptr(&state).cast_mut().cast::<c_void>();
    // SAFETY: `state` and callback meet `HostApi::new`'s lifetime contract.
    let host = unsafe { HostApi::new(handle, host_post_frame) };
    let mut plugin = PluginApi::EMPTY;
    assert_eq!(
        // SAFETY: Both ABI tables have their advertised storage.
        unsafe {
            rsi_meta_plugin::sdk::initialize::<FailOnceShutdownPlugin>(
                &raw const host,
                &raw mut plugin,
                core::mem::size_of::<PluginApi>(),
            )
        },
        INIT_OK
    );
    let shutdown = plugin.shutdown.expect("shutdown callback");
    // SAFETY: The plugin handle remains live until the one destroy call below.
    assert_eq!(
        unsafe { shutdown(plugin.plugin_handle) },
        rsi_meta_plugin::CALL_FAILED
    );
    // SAFETY: A failed graceful shutdown does not consume the retry.
    assert_eq!(unsafe { shutdown(plugin.plugin_handle) }, CALL_OK);
    assert_eq!(SHUTDOWN_ATTEMPTS.load(Ordering::SeqCst), 2);
    // SAFETY: This is the single destroy call for the live handle.
    assert_eq!(
        unsafe { plugin.destroy.unwrap()(plugin.plugin_handle) },
        CALL_OK
    );
}
