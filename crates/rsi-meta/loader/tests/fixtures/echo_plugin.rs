#![allow(unsafe_code)] // This fixture implements the raw ABI tested by the loader.

use core::ffi::c_void;
use std::convert::Infallible;
use std::sync::atomic::{AtomicU32, Ordering};
use std::thread::JoinHandle;

use rsi_meta_plugin::sdk::{Host, Plugin};
use rsi_meta_plugin::{
    ABI_MAJOR, ABI_MINOR, HostApi, INIT_INVALID_HOST_API, INIT_OK, Lane, PluginApi,
};

struct EchoPlugin {
    host: Host,
    workers: Vec<JoinHandle<()>>,
}

impl Plugin for EchoPlugin {
    type Error = Infallible;

    fn create(host: Host) -> Result<Self, Self::Error> {
        Ok(Self {
            host,
            workers: Vec::new(),
        })
    }

    fn on_frame(&mut self, lane: Lane, payload: &[u8]) -> Result<(), Self::Error> {
        if let Some(payload) = payload.strip_prefix(b"unsolicited:") {
            let host = self.host;
            let payload = payload.to_vec();
            self.workers.push(std::thread::spawn(move || {
                let _ = host.post_frame(lane, &payload);
            }));
        } else if let Some(payload) = payload.strip_prefix(b"thread:") {
            let host = self.host;
            let payload = payload.to_vec();
            std::thread::spawn(move || {
                let _ = host.post_frame(lane, &payload);
            })
            .join()
            .expect("fixture callback thread");
        } else {
            let _ = self.host.post_frame(lane, payload);
        }
        Ok(())
    }

    fn shutdown(&mut self) -> Result<(), Self::Error> {
        for worker in self.workers.drain(..) {
            let _ = worker.join();
        }
        Ok(())
    }
}

/// Setting the host's reserved word to this value makes the fixture advertise
/// an intentionally incompatible plugin table.
pub const BAD_ABI_MARKER: u32 = 0xBAD0_AB10;

static BAD_ABI_DESTROY_CALLS: AtomicU32 = AtomicU32::new(0);

unsafe extern "C" fn incompatible_destroy(_plugin_handle: *mut c_void) -> u32 {
    BAD_ABI_DESTROY_CALLS.fetch_add(1, Ordering::SeqCst);
    0
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rsi_meta_plugin_entry_v0(
    host_api: *const HostApi,
    plugin_api_out: *mut PluginApi,
    plugin_api_capacity: usize,
) -> u32 {
    if host_api.is_null()
        || plugin_api_out.is_null()
        || plugin_api_capacity < core::mem::size_of::<PluginApi>()
    {
        return INIT_INVALID_HOST_API;
    }
    // SAFETY: The test host context starts with an AtomicU32 marker and remains
    // live for this call. Production hosts never use this fixture-only branch.
    let marker = unsafe {
        (*host_api)
            .host_handle
            .cast::<AtomicU32>()
            .as_ref()
            .map_or(0, |marker| marker.load(Ordering::SeqCst))
    };
    if marker == BAD_ABI_MARKER {
        // SAFETY: PluginLoader supplies writable storage for a complete table.
        unsafe {
            plugin_api_out.write(PluginApi {
                abi_major: ABI_MAJOR + 1,
                abi_minor: ABI_MINOR,
                struct_size: PluginApi::STRUCT_SIZE,
                reserved: 0,
                plugin_handle: std::ptr::dangling_mut::<c_void>(),
                on_frame: None,
                shutdown: None,
                destroy: Some(incompatible_destroy),
            });
        }
        return INIT_OK;
    }

    // SAFETY: The loader owns the input/output table contract; the SDK validates
    // the host prefix and catches plugin panics before returning across the ABI.
    unsafe {
        rsi_meta_plugin::sdk::initialize::<EchoPlugin>(
            host_api,
            plugin_api_out,
            plugin_api_capacity,
        )
    }
}

static RESIDENCY_COUNTER: AtomicU32 = AtomicU32::new(0);

#[unsafe(no_mangle)]
pub extern "C" fn rsi_meta_fixture_next_counter() -> u32 {
    RESIDENCY_COUNTER.fetch_add(1, Ordering::SeqCst) + 1
}

#[unsafe(no_mangle)]
pub extern "C" fn rsi_meta_fixture_bad_destroy_calls() -> u32 {
    BAD_ABI_DESTROY_CALLS.load(Ordering::SeqCst)
}
