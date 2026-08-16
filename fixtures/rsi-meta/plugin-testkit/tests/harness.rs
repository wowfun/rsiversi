use std::convert::Infallible;
use std::ffi::c_void;
use std::time::Duration;

use rsi_meta_frame_contract::{Frame, FrameBody};
use rsi_meta_plugin::sdk::{Host, Plugin};
use rsi_meta_plugin::{
    ABI_MAJOR, ABI_MINOR, CALL_FAILED, CALL_OK, CallOutcome, HostApi, INIT_OK, Lane, PluginApi,
    PostFrameOutcome,
};
use rsi_meta_plugin_testkit::PluginHarness;
use serde_json::json;

struct Echo {
    host: Host,
}

impl Plugin for Echo {
    type Error = Infallible;

    fn create(host: Host) -> Result<Self, Self::Error> {
        Ok(Self { host })
    }

    fn on_frame(&mut self, lane: Lane, payload: &[u8]) -> Result<(), Self::Error> {
        let _ = self.host.post_frame(lane, payload);
        Ok(())
    }
}

rsi_meta_plugin::export_plugin!(Echo);

#[test]
fn harness_drives_only_the_public_abi_and_captures_posted_frames() {
    let mut harness = PluginHarness::start(rsi_meta_plugin_entry_v0).unwrap();
    let input = Frame::service_request("r-1", "fixture.echo", "echo", json!({"value": 7}));

    assert_eq!(harness.send(Lane::Data, &input).unwrap(), CallOutcome::Ok);
    let output = harness.recv(Duration::from_secs(1)).unwrap();
    assert_eq!(output.lane, Lane::Data);
    assert!(matches!(
        output.frame.body,
        FrameBody::ServiceRequest { ref request_id, .. } if request_id == "r-1"
    ));
    assert!(harness.try_recv().unwrap().is_none());
}

#[test]
fn harness_can_script_successive_post_outcomes() {
    let mut harness = PluginHarness::start(rsi_meta_plugin_entry_v0).unwrap();
    harness.set_post_outcomes([PostFrameOutcome::WouldBlock, PostFrameOutcome::Accepted]);

    let first = Frame::service_request("blocked", "fixture.echo", "echo", json!({}));
    let second = Frame::service_request("accepted", "fixture.echo", "echo", json!({}));
    assert_eq!(harness.send(Lane::Data, &first).unwrap(), CallOutcome::Ok);
    assert!(harness.try_recv().unwrap().is_none());
    assert_eq!(harness.send(Lane::Data, &second).unwrap(), CallOutcome::Ok);
    assert!(matches!(
        harness.recv(Duration::from_secs(1)).unwrap().frame.body,
        FrameBody::ServiceRequest { request_id, .. } if request_id == "accepted"
    ));
}

struct RetainedHostTable(*const HostApi);

unsafe extern "C" fn retained_on_frame(
    _handle: *mut c_void,
    _lane: u32,
    _data: *const u8,
    _length: usize,
) -> u32 {
    CALL_OK
}

unsafe extern "C" fn retained_shutdown(_handle: *mut c_void) -> u32 {
    CALL_OK
}

unsafe extern "C" fn retained_destroy(handle: *mut c_void) -> u32 {
    // SAFETY: The entry point allocated this exact handle and the ABI permits
    // one destroy call.
    let retained = unsafe { Box::from_raw(handle.cast::<RetainedHostTable>()) };
    // SAFETY: A conforming host table must remain readable through destroy.
    let host = unsafe { &*retained.0 };
    // SAFETY: Zero-length callback payloads permit a null data pointer.
    let Some(post) = host.host_post_frame else {
        return CALL_FAILED;
    };
    // SAFETY: The compatible retained host table owns this callback and handle
    // through the destroy call; a zero-length payload permits a null pointer.
    let _ = unsafe {
        post(
            host.host_handle,
            Lane::Control.as_raw(),
            std::ptr::null(),
            0,
        )
    };
    CALL_FAILED
}

struct PanickingPlugin;

impl Plugin for PanickingPlugin {
    type Error = Infallible;

    fn create(_host: Host) -> Result<Self, Self::Error> {
        Ok(Self)
    }

    fn on_frame(&mut self, _lane: Lane, _payload: &[u8]) -> Result<(), Self::Error> {
        panic!("fixture panic must stop at the SDK trampoline")
    }
}

unsafe extern "C" fn panicking_entry(
    host: *const HostApi,
    out: *mut PluginApi,
    capacity: usize,
) -> u32 {
    // SAFETY: PluginHarness supplies the same entry-point contract used by the
    // exported plugin macro.
    unsafe { rsi_meta_plugin::sdk::initialize::<PanickingPlugin>(host, out, capacity) }
}

#[test]
fn harness_observes_sdk_panic_containment_without_unwinding_the_abi() {
    let mut harness = PluginHarness::start(panicking_entry).unwrap();
    let frame = Frame::service_request("panic", "fixture.echo", "panic", json!({}));

    assert_eq!(
        harness.send(Lane::Data, &frame).unwrap(),
        CallOutcome::Panicked
    );
}

unsafe extern "C" fn retain_host_entry(
    host: *const HostApi,
    out: *mut PluginApi,
    capacity: usize,
) -> u32 {
    if capacity < core::mem::size_of::<PluginApi>() {
        return rsi_meta_plugin::INIT_INVALID_HOST_API;
    }
    let retained = Box::new(RetainedHostTable(host));
    // SAFETY: The harness supplies writable storage for a complete PluginApi.
    unsafe {
        *out = PluginApi {
            abi_major: ABI_MAJOR,
            abi_minor: ABI_MINOR,
            struct_size: PluginApi::STRUCT_SIZE,
            reserved: 0,
            plugin_handle: Box::into_raw(retained).cast(),
            on_frame: Some(retained_on_frame),
            shutdown: Some(retained_shutdown),
            destroy: Some(retained_destroy),
        };
    }
    INIT_OK
}

#[test]
fn harness_keeps_the_host_table_alive_through_a_failing_destroy_callback() {
    let harness = PluginHarness::start(retain_host_entry).unwrap();
    drop(harness);
}
