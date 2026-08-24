#![allow(unsafe_code)] // The test exercises the package's audited raw ABI.

use rsi_meta_plugin::{
    ABI_MAJOR, ABI_MINOR, Buffer, HostApi, NativeInstance, NativePlugin, PluginApi, STATUS_FAILED,
    STATUS_OK, STATUS_PANICKED, copy_buffer, plugin_api,
};
use serde_json::{Value, json};
#[cfg(any(target_os = "linux", target_os = "macos"))]
use std::fs;
#[cfg(any(target_os = "linux", target_os = "macos"))]
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

#[derive(Default)]
struct Echo;

impl NativePlugin for Echo {
    type Instance = EchoInstance;

    fn descriptor(&self) -> Value {
        json!({
            "identity": { "kind": "builtin", "name": "ignored", "revision": "ignored" },
            "requires": [],
            "provides": [{ "key": "echo", "contract": "test.echo", "version": 1 }]
        })
    }

    fn create(&self, _: Value) -> Result<Self::Instance, String> {
        Ok(EchoInstance)
    }
}

struct EchoInstance;

impl NativeInstance for EchoInstance {
    fn call(
        &mut self,
        _: &rsi_meta_plugin::Host<'_>,
        service: &str,
        request: &[u8],
    ) -> Result<Vec<u8>, String> {
        assert_eq!(service, "echo");
        Ok(request.to_vec())
    }
}

#[test]
fn table_layout_and_sdk_ownership_round_trip() {
    let pointer = size_of::<*mut core::ffi::c_void>();
    assert_eq!(core::mem::offset_of!(Buffer, ptr), 0);
    assert_eq!(core::mem::offset_of!(Buffer, len), pointer);
    assert_eq!(core::mem::offset_of!(Buffer, capacity), 2 * pointer);
    assert_eq!(size_of::<Buffer>(), 3 * pointer);

    assert_eq!(core::mem::offset_of!(HostApi, abi_major), 0);
    assert_eq!(core::mem::offset_of!(HostApi, abi_minor), 4);
    assert_eq!(core::mem::offset_of!(HostApi, struct_size), 8);
    assert_eq!(core::mem::offset_of!(HostApi, reserved), 12);
    assert_eq!(core::mem::offset_of!(HostApi, host_handle), 16);
    assert_eq!(core::mem::offset_of!(HostApi, call_service), 16 + pointer);
    assert_eq!(
        core::mem::offset_of!(HostApi, release_buffer),
        16 + 2 * pointer
    );
    assert_eq!(size_of::<HostApi>(), 16 + 3 * pointer);
    assert_eq!(HostApi::MIN_SIZE_V1_0 as usize, 16 + 3 * pointer);
    assert_eq!(HostApi::STRUCT_SIZE as usize, 16 + 3 * pointer);

    assert_eq!(core::mem::offset_of!(PluginApi, abi_major), 0);
    assert_eq!(core::mem::offset_of!(PluginApi, abi_minor), 4);
    assert_eq!(core::mem::offset_of!(PluginApi, struct_size), 8);
    assert_eq!(core::mem::offset_of!(PluginApi, reserved), 12);
    assert_eq!(core::mem::offset_of!(PluginApi, factory_handle), 16);
    assert_eq!(core::mem::offset_of!(PluginApi, descriptor), 16 + pointer);
    assert_eq!(
        core::mem::offset_of!(PluginApi, validate_config),
        16 + 2 * pointer
    );
    assert_eq!(core::mem::offset_of!(PluginApi, create), 16 + 3 * pointer);
    assert_eq!(core::mem::offset_of!(PluginApi, call), 16 + 4 * pointer);
    assert_eq!(
        core::mem::offset_of!(PluginApi, destroy_instance),
        16 + 5 * pointer
    );
    assert_eq!(
        core::mem::offset_of!(PluginApi, destroy_factory),
        16 + 6 * pointer
    );
    assert_eq!(
        core::mem::offset_of!(PluginApi, release_buffer),
        16 + 7 * pointer
    );
    assert_eq!(size_of::<PluginApi>(), 16 + 8 * pointer);
    assert_eq!(PluginApi::MIN_SIZE_V1_0 as usize, 16 + 8 * pointer);
    assert_eq!(PluginApi::STRUCT_SIZE as usize, 16 + 8 * pointer);
    let api = plugin_api::<Echo>();
    assert_eq!((api.abi_major, api.abi_minor), (ABI_MAJOR, ABI_MINOR));
    assert!(api.is_compatible());

    let mut descriptor = Buffer::EMPTY;
    // SAFETY: api is live and output storage is valid.
    let status = unsafe { api.descriptor.unwrap()(api.factory_handle, &raw mut descriptor) };
    assert_eq!(status, STATUS_OK);
    // SAFETY: The plugin owns a readable buffer until release.
    let value: Value = serde_json::from_slice(&unsafe { copy_buffer(descriptor) }).unwrap();
    assert_eq!(value["provides"][0]["key"], "echo");
    // SAFETY: Matching allocator callback, exactly once.
    unsafe { api.release_buffer.unwrap()(descriptor) };
    // SAFETY: Matching factory destructor, exactly once.
    unsafe { api.destroy_factory.unwrap()(api.factory_handle) };
}

#[test]
fn minor_versions_are_compatible_in_the_extension_direction() {
    let mut future_host = HostApi {
        abi_major: ABI_MAJOR,
        abi_minor: ABI_MINOR + 1,
        struct_size: HostApi::STRUCT_SIZE,
        reserved: 0,
        host_handle: core::ptr::dangling_mut::<core::ffi::c_void>(),
        call_service: Some(dummy_host_call),
        release_buffer: Some(dummy_release),
    };
    assert!(future_host.is_compatible());

    future_host.struct_size = HostApi::MIN_SIZE_V1_0;
    assert!(future_host.is_compatible());

    let mut future_plugin = plugin_api::<Echo>();
    future_plugin.struct_size = PluginApi::MIN_SIZE_V1_0;
    assert!(future_plugin.is_compatible());
    future_plugin.abi_minor = ABI_MINOR + 1;
    assert!(!future_plugin.is_compatible());

    future_host.abi_major += 1;
    assert!(!future_host.is_compatible());

    // SAFETY: This factory handle came from plugin_api and is destroyed once.
    unsafe { future_plugin.destroy_factory.unwrap()(future_plugin.factory_handle) };
}

unsafe extern "C" fn dummy_host_call(
    _: *mut core::ffi::c_void,
    _: *const u8,
    _: usize,
    _: *const u8,
    _: usize,
    _: *mut Buffer,
) -> u32 {
    STATUS_FAILED
}

unsafe extern "C" fn dummy_release(_: Buffer) {}

#[derive(Default)]
struct HostBufferProbe;

impl NativePlugin for HostBufferProbe {
    type Instance = HostBufferProbeInstance;

    fn descriptor(&self) -> Value {
        Value::Null
    }

    fn create(&self, _: Value) -> Result<Self::Instance, String> {
        Ok(HostBufferProbeInstance)
    }
}

struct HostBufferProbeInstance;

impl NativeInstance for HostBufferProbeInstance {
    fn call(
        &mut self,
        host: &rsi_meta_plugin::Host<'_>,
        _: &str,
        _: &[u8],
    ) -> Result<Vec<u8>, String> {
        host.call("malformed", b"request")
    }
}

static MALFORMED_HOST_RELEASES: AtomicUsize = AtomicUsize::new(0);

unsafe extern "C" fn malformed_host_call(
    _: *mut core::ffi::c_void,
    _: *const u8,
    _: usize,
    _: *const u8,
    _: usize,
    output: *mut Buffer,
) -> u32 {
    // SAFETY: The ABI caller supplied exclusive output storage for this call.
    unsafe {
        output.write(Buffer {
            ptr: core::ptr::null_mut(),
            len: 1,
            capacity: 1,
        });
    }
    STATUS_OK
}

unsafe extern "C" fn count_malformed_host_release(_: Buffer) {
    MALFORMED_HOST_RELEASES.fetch_add(1, Ordering::AcqRel);
}

#[test]
fn sdk_rejects_malformed_host_buffers_and_releases_them_once() {
    MALFORMED_HOST_RELEASES.store(0, Ordering::Release);
    let api = plugin_api::<HostBufferProbe>();
    let config = b"null";
    let mut instance = core::ptr::null_mut();
    let mut create_error = Buffer::EMPTY;
    // SAFETY: All inputs and output slots remain live for the synchronous call.
    assert_eq!(
        unsafe {
            api.create.unwrap()(
                api.factory_handle,
                config.as_ptr(),
                config.len(),
                &raw mut instance,
                &raw mut create_error,
            )
        },
        STATUS_OK
    );
    let host = HostApi {
        abi_major: ABI_MAJOR,
        abi_minor: ABI_MINOR,
        struct_size: HostApi::STRUCT_SIZE,
        reserved: 0,
        host_handle: core::ptr::dangling_mut::<core::ffi::c_void>(),
        call_service: Some(malformed_host_call),
        release_buffer: Some(count_malformed_host_release),
    };
    let service = b"probe";
    let mut output = Buffer::EMPTY;
    // SAFETY: The tables, instance, inputs, and output slot remain live.
    let status = unsafe {
        api.call.unwrap()(
            instance,
            &raw const host,
            service.as_ptr(),
            service.len(),
            core::ptr::null(),
            0,
            &raw mut output,
        )
    };
    assert_eq!(status, STATUS_FAILED);
    assert_eq!(MALFORMED_HOST_RELEASES.load(Ordering::Acquire), 1);
    // SAFETY: Plugin-owned buffers and handles are returned exactly once.
    unsafe {
        api.release_buffer.unwrap()(output);
        api.destroy_instance.unwrap()(instance);
        api.destroy_factory.unwrap()(api.factory_handle);
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn maintained_header_compiles_with_c11_and_cpp17_layout_assertions() {
    let directory = tempfile::tempdir().unwrap();
    let assertions = r#"
#include <stddef.h>
#include "rsi_meta_plugin.h"

#if defined(__cplusplus)
#define RSI_META_ASSERT static_assert
#else
#define RSI_META_ASSERT _Static_assert
#endif

RSI_META_ASSERT(RSI_META_ABI_MAJOR == 1u, "ABI major drift");
RSI_META_ASSERT(RSI_META_ABI_MINOR == 0u, "ABI minor drift");
RSI_META_ASSERT(offsetof(rsi_meta_buffer, ptr) == 0, "buffer ptr offset");
RSI_META_ASSERT(offsetof(rsi_meta_buffer, len) == sizeof(void *), "buffer len offset");
RSI_META_ASSERT(sizeof(rsi_meta_buffer) == 3 * sizeof(void *), "buffer size");
RSI_META_ASSERT(offsetof(rsi_meta_host_api, host_handle) == 16, "host handle offset");
RSI_META_ASSERT(offsetof(rsi_meta_host_api, call_service) == 16 + sizeof(void *), "host call offset");
RSI_META_ASSERT(offsetof(rsi_meta_host_api, release_buffer) == 16 + 2 * sizeof(void *), "host release offset");
RSI_META_ASSERT(sizeof(rsi_meta_host_api) == 16 + 3 * sizeof(void *), "host size");
RSI_META_ASSERT(offsetof(rsi_meta_plugin_api, factory_handle) == 16, "plugin handle offset");
RSI_META_ASSERT(offsetof(rsi_meta_plugin_api, descriptor) == 16 + sizeof(void *), "descriptor offset");
RSI_META_ASSERT(offsetof(rsi_meta_plugin_api, validate_config) == 16 + 2 * sizeof(void *), "validate offset");
RSI_META_ASSERT(offsetof(rsi_meta_plugin_api, create) == 16 + 3 * sizeof(void *), "create offset");
RSI_META_ASSERT(offsetof(rsi_meta_plugin_api, call) == 16 + 4 * sizeof(void *), "call offset");
RSI_META_ASSERT(offsetof(rsi_meta_plugin_api, destroy_instance) == 16 + 5 * sizeof(void *), "instance destroy offset");
RSI_META_ASSERT(offsetof(rsi_meta_plugin_api, destroy_factory) == 16 + 6 * sizeof(void *), "factory destroy offset");
RSI_META_ASSERT(offsetof(rsi_meta_plugin_api, release_buffer) == 16 + 7 * sizeof(void *), "plugin release offset");
RSI_META_ASSERT(sizeof(rsi_meta_plugin_api) == 16 + 8 * sizeof(void *), "plugin size");

typedef uint32_t (*entry_fn)(rsi_meta_plugin_api *, size_t);
static entry_fn typed_entry = rsi_meta_plugin_entry_v1;
int use_entry_type(void) { return typed_entry == NULL; }
"#;
    for (extension, compiler_variable, fallback, standard) in
        [("c", "CC", "cc", "c11"), ("cc", "CXX", "c++", "c++17")]
    {
        let source = directory.path().join(format!("abi_layout.{extension}"));
        let object = directory.path().join(format!("abi_layout.{extension}.o"));
        fs::write(&source, assertions).unwrap();
        let compiler = std::env::var_os(compiler_variable).unwrap_or_else(|| fallback.into());
        let status = Command::new(compiler)
            .arg(format!("-std={standard}"))
            .args(["-Wall", "-Wextra", "-Werror", "-pedantic", "-c"])
            .arg(&source)
            .arg("-I")
            .arg(concat!(env!("CARGO_MANIFEST_DIR"), "/include"))
            .arg("-o")
            .arg(object)
            .status()
            .expect("invoke host C-family compiler");
        assert!(
            status.success(),
            "{standard} rejected the maintained header"
        );
    }
}

#[test]
fn null_zero_inputs_are_handled_without_constructing_null_slices() {
    let api = plugin_api::<Echo>();
    let mut validation = Buffer::EMPTY;
    // SAFETY: Null is explicitly valid for a zero-length ABI input.
    let status = unsafe {
        api.validate_config.unwrap()(
            api.factory_handle,
            core::ptr::null(),
            0,
            &raw mut validation,
        )
    };
    assert_eq!(status, STATUS_FAILED);
    // SAFETY: Matching allocator callback, exactly once.
    unsafe { api.release_buffer.unwrap()(validation) };

    let config = b"null";
    let mut instance = core::ptr::null_mut();
    let mut error = Buffer::EMPTY;
    // SAFETY: All borrows and output slots remain valid for the call.
    assert_eq!(
        unsafe {
            api.create.unwrap()(
                api.factory_handle,
                config.as_ptr(),
                config.len(),
                &raw mut instance,
                &raw mut error,
            )
        },
        STATUS_OK
    );
    let mut output = Buffer::EMPTY;
    let host = HostApi {
        abi_major: 0,
        abi_minor: 0,
        struct_size: 0,
        reserved: 0,
        host_handle: core::ptr::null_mut(),
        call_service: None,
        release_buffer: None,
    };
    let service = b"echo";
    // SAFETY: Null is explicitly valid for the empty request; other inputs and
    // output storage remain live. The deliberately incompatible host is unused.
    let status = unsafe {
        api.call.unwrap()(
            instance,
            &raw const host,
            service.as_ptr(),
            service.len(),
            core::ptr::null(),
            0,
            &raw mut output,
        )
    };
    assert_eq!(status, STATUS_FAILED);
    // SAFETY: Each allocation is returned to its matching callback once.
    unsafe {
        api.release_buffer.unwrap()(output);
        api.destroy_instance.unwrap()(instance);
        api.destroy_factory.unwrap()(api.factory_handle);
    }
}

struct PanickingDefault;

impl Default for PanickingDefault {
    fn default() -> Self {
        panic!("default panic evidence")
    }
}

impl NativePlugin for PanickingDefault {
    type Instance = EchoInstance;

    fn descriptor(&self) -> Value {
        Value::Null
    }

    fn create(&self, _: Value) -> Result<Self::Instance, String> {
        Ok(EchoInstance)
    }
}

#[test]
fn panicking_factory_construction_returns_an_incompatible_empty_table() {
    assert!(!plugin_api::<PanickingDefault>().is_compatible());
}

#[derive(Default)]
struct PanickingDrops;

impl Drop for PanickingDrops {
    fn drop(&mut self) {
        panic!("factory drop panic evidence")
    }
}

impl NativePlugin for PanickingDrops {
    type Instance = PanickingInstanceDrop;

    fn descriptor(&self) -> Value {
        Value::Null
    }

    fn create(&self, _: Value) -> Result<Self::Instance, String> {
        Ok(PanickingInstanceDrop)
    }
}

struct PanickingInstanceDrop;

impl Drop for PanickingInstanceDrop {
    fn drop(&mut self) {
        panic!("instance drop panic evidence")
    }
}

impl NativeInstance for PanickingInstanceDrop {
    fn call(
        &mut self,
        _: &rsi_meta_plugin::Host<'_>,
        _: &str,
        _: &[u8],
    ) -> Result<Vec<u8>, String> {
        Ok(Vec::new())
    }
}

#[test]
fn panicking_destructors_do_not_unwind_across_the_c_abi() {
    let api = plugin_api::<PanickingDrops>();
    let config = b"null";
    let mut instance = core::ptr::null_mut();
    let mut error = Buffer::EMPTY;
    // SAFETY: Inputs and output slots remain live for this synchronous call.
    assert_eq!(
        unsafe {
            api.create.unwrap()(
                api.factory_handle,
                config.as_ptr(),
                config.len(),
                &raw mut instance,
                &raw mut error,
            )
        },
        STATUS_OK
    );
    // SAFETY: Both handles have exact allocator provenance and are destroyed once.
    unsafe {
        api.destroy_instance.unwrap()(instance);
        api.destroy_factory.unwrap()(api.factory_handle);
    }
}

#[derive(Default)]
struct PanickingCallbacks;

impl NativePlugin for PanickingCallbacks {
    type Instance = PanickingCall;

    fn descriptor(&self) -> Value {
        panic!("descriptor panic evidence")
    }

    fn validate_config(&self, _: Value) -> Result<Value, String> {
        panic!("configuration panic evidence")
    }

    fn create(&self, config: Value) -> Result<Self::Instance, String> {
        assert!(config != Value::Bool(true), "create panic evidence");
        Ok(PanickingCall)
    }
}

struct PanickingCall;

impl NativeInstance for PanickingCall {
    fn call(
        &mut self,
        _: &rsi_meta_plugin::Host<'_>,
        _: &str,
        _: &[u8],
    ) -> Result<Vec<u8>, String> {
        panic!("call panic evidence")
    }
}

fn assert_empty(buffer: Buffer) {
    assert!(buffer.ptr.is_null());
    assert_eq!((buffer.len, buffer.capacity), (0, 0));
}

#[test]
fn panicking_callbacks_return_status_without_publishing_outputs() {
    let api = plugin_api::<PanickingCallbacks>();
    let mut output = Buffer::EMPTY;
    // SAFETY: The table and output slot remain valid for the synchronous call.
    assert_eq!(
        unsafe { api.descriptor.unwrap()(api.factory_handle, &raw mut output) },
        STATUS_PANICKED
    );
    assert_empty(output);

    let null = b"null";
    // SAFETY: Input and output storage remain valid for the synchronous call.
    assert_eq!(
        unsafe {
            api.validate_config.unwrap()(
                api.factory_handle,
                null.as_ptr(),
                null.len(),
                &raw mut output,
            )
        },
        STATUS_PANICKED
    );
    assert_empty(output);

    let should_panic = b"true";
    let mut instance = core::ptr::null_mut();
    // SAFETY: Input and both output slots remain valid for the synchronous call.
    assert_eq!(
        unsafe {
            api.create.unwrap()(
                api.factory_handle,
                should_panic.as_ptr(),
                should_panic.len(),
                &raw mut instance,
                &raw mut output,
            )
        },
        STATUS_PANICKED
    );
    assert!(instance.is_null());
    assert_empty(output);

    // SAFETY: Valid inputs and output slots remain live; this non-panicking
    // create transfers one instance to the test.
    assert_eq!(
        unsafe {
            api.create.unwrap()(
                api.factory_handle,
                null.as_ptr(),
                null.len(),
                &raw mut instance,
                &raw mut output,
            )
        },
        STATUS_OK
    );
    let host = HostApi {
        abi_major: ABI_MAJOR,
        abi_minor: ABI_MINOR,
        struct_size: HostApi::STRUCT_SIZE,
        reserved: 0,
        host_handle: core::ptr::dangling_mut::<core::ffi::c_void>(),
        call_service: Some(dummy_host_call),
        release_buffer: Some(dummy_release),
    };
    let service = b"panic";
    // SAFETY: The instance has create provenance and all borrowed inputs and
    // output storage remain valid for the synchronous call.
    assert_eq!(
        unsafe {
            api.call.unwrap()(
                instance,
                &raw const host,
                service.as_ptr(),
                service.len(),
                core::ptr::null(),
                0,
                &raw mut output,
            )
        },
        STATUS_PANICKED
    );
    assert_empty(output);

    // SAFETY: Both handles have exact allocator provenance and are destroyed once.
    unsafe {
        api.destroy_instance.unwrap()(instance);
        api.destroy_factory.unwrap()(api.factory_handle);
    }
}
