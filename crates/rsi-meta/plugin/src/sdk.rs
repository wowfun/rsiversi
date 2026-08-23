use super::{
    ABI_MAJOR, ABI_MINOR, Buffer, HostApi, PluginApi, STATUS_FAILED, STATUS_INVALID_ARGUMENT,
    STATUS_OK, STATUS_PANICKED,
};
use core::ffi::c_void;
use serde_json::Value;
use std::panic::{AssertUnwindSafe, catch_unwind};

mod host;

pub use host::Host;

pub trait NativePlugin: Default + Send + Sync + 'static {
    type Instance: NativeInstance;

    fn descriptor(&self) -> Value;
    fn validate_config(&self, config: Value) -> Result<Value, String> {
        Ok(config)
    }
    fn create(&self, config: Value) -> Result<Self::Instance, String>;
}

pub trait NativeInstance: Send + 'static {
    fn call(&mut self, host: &Host<'_>, service: &str, request: &[u8]) -> Result<Vec<u8>, String>;
}

/// Copies an ABI buffer without taking allocator ownership.
///
/// # Safety
///
/// A nonempty buffer must point to `len` readable bytes for this call.
pub unsafe fn copy_buffer(buffer: Buffer) -> Vec<u8> {
    if buffer.len == 0 {
        return Vec::new();
    }
    assert!(
        !buffer.ptr.is_null(),
        "nonempty ABI buffer has a null pointer"
    );
    // SAFETY: The caller guarantees exact readable bounds.
    unsafe { std::slice::from_raw_parts(buffer.ptr, buffer.len) }.to_vec()
}

/// Borrows an ABI input pair without constructing a slice from a null pointer.
///
/// # Safety
///
/// A nonempty input must point to exactly `len` readable bytes for the returned
/// borrow. The caller controls the borrow lifetime.
#[doc(hidden)]
pub unsafe fn borrow_abi_input<'a>(ptr: *const u8, len: usize) -> &'a [u8] {
    if len == 0 {
        &[]
    } else {
        // SAFETY: The caller guarantees the nonempty pointer's readable bounds.
        unsafe { std::slice::from_raw_parts(ptr, len) }
    }
}

/// Builds the table used by [`export_plugin!`].
#[doc(hidden)]
#[allow(clippy::too_many_lines)] // Each ABI callback stays adjacent to table construction.
pub fn plugin_api<P: NativePlugin>() -> PluginApi {
    unsafe extern "C" fn descriptor<P: NativePlugin>(
        handle: *mut c_void,
        output: *mut Buffer,
    ) -> u32 {
        if handle.is_null() || output.is_null() {
            return STATUS_INVALID_ARGUMENT;
        }
        let result = catch_unwind(AssertUnwindSafe(|| {
            // SAFETY: The entry trampoline allocated this P and it remains live.
            let plugin = unsafe { &*handle.cast::<P>() };
            serde_json::to_vec(&plugin.descriptor())
        }));
        match result {
            Ok(Ok(bytes)) => {
                // SAFETY: output is non-null and exclusively borrowed.
                unsafe { output.write(Buffer::from_vec(bytes)) };
                STATUS_OK
            }
            Ok(Err(error)) => {
                // SAFETY: output is non-null and exclusively borrowed.
                unsafe { output.write(Buffer::from_vec(error.to_string().into_bytes())) };
                STATUS_FAILED
            }
            Err(_) => STATUS_PANICKED,
        }
    }

    unsafe extern "C" fn create<P: NativePlugin>(
        handle: *mut c_void,
        config_ptr: *const u8,
        config_len: usize,
        instance_out: *mut *mut c_void,
        error_out: *mut Buffer,
    ) -> u32 {
        if handle.is_null()
            || instance_out.is_null()
            || error_out.is_null()
            || (config_ptr.is_null() && config_len != 0)
        {
            return STATUS_INVALID_ARGUMENT;
        }
        let result = catch_unwind(AssertUnwindSafe(|| {
            // SAFETY: The checked pointer/length is borrowed synchronously.
            let bytes = unsafe { borrow_abi_input(config_ptr, config_len) };
            let config = serde_json::from_slice(bytes).map_err(|error| error.to_string())?;
            // SAFETY: handle has factory provenance and remains live.
            unsafe { &*handle.cast::<P>() }.create(config)
        }));
        match result {
            Ok(Ok(instance)) => {
                // SAFETY: instance_out is non-null and exclusively borrowed.
                unsafe { instance_out.write(Box::into_raw(Box::new(instance)).cast()) };
                STATUS_OK
            }
            Ok(Err(error)) => {
                // SAFETY: error_out is non-null and exclusively borrowed.
                unsafe { error_out.write(Buffer::from_vec(error.into_bytes())) };
                STATUS_FAILED
            }
            Err(_) => STATUS_PANICKED,
        }
    }

    unsafe extern "C" fn validate_config<P: NativePlugin>(
        handle: *mut c_void,
        config_ptr: *const u8,
        config_len: usize,
        output: *mut Buffer,
    ) -> u32 {
        if handle.is_null() || output.is_null() || (config_ptr.is_null() && config_len != 0) {
            return STATUS_INVALID_ARGUMENT;
        }
        let result = catch_unwind(AssertUnwindSafe(|| {
            // SAFETY: The checked pointer/length is borrowed synchronously.
            let bytes = unsafe { borrow_abi_input(config_ptr, config_len) };
            let config = serde_json::from_slice(bytes).map_err(|error| error.to_string())?;
            // SAFETY: handle has factory provenance and remains live.
            let config = unsafe { &*handle.cast::<P>() }.validate_config(config)?;
            serde_json::to_vec(&config).map_err(|error| error.to_string())
        }));
        match result {
            Ok(Ok(bytes)) => {
                // SAFETY: output is non-null and exclusively borrowed.
                unsafe { output.write(Buffer::from_vec(bytes)) };
                STATUS_OK
            }
            Ok(Err(error)) => {
                // SAFETY: output is non-null and exclusively borrowed.
                unsafe { output.write(Buffer::from_vec(error.into_bytes())) };
                STATUS_FAILED
            }
            Err(_) => STATUS_PANICKED,
        }
    }

    unsafe extern "C" fn call<P: NativePlugin>(
        instance: *mut c_void,
        host: *const HostApi,
        service_ptr: *const u8,
        service_len: usize,
        request_ptr: *const u8,
        request_len: usize,
        output: *mut Buffer,
    ) -> u32 {
        if instance.is_null()
            || host.is_null()
            || output.is_null()
            || (service_ptr.is_null() && service_len != 0)
            || (request_ptr.is_null() && request_len != 0)
        {
            return STATUS_INVALID_ARGUMENT;
        }
        let result = catch_unwind(AssertUnwindSafe(|| {
            // SAFETY: Input borrows are valid for this synchronous callback.
            let service = unsafe { borrow_abi_input(service_ptr, service_len) };
            let service = std::str::from_utf8(service).map_err(|error| error.to_string())?;
            // SAFETY: Input borrows are valid for this synchronous callback.
            let request = unsafe { borrow_abi_input(request_ptr, request_len) };
            // SAFETY: The checked host table remains live for this callback.
            let host_api = unsafe { &*host };
            let host = Host::new(host_api);
            if !host_api.is_compatible() {
                return Err("incompatible host API".to_owned());
            }
            // SAFETY: Host serializes calls and create allocated this instance.
            unsafe { &mut *instance.cast::<P::Instance>() }.call(&host, service, request)
        }));
        match result {
            Ok(Ok(bytes)) => {
                // SAFETY: output is non-null and exclusively borrowed.
                unsafe { output.write(Buffer::from_vec(bytes)) };
                STATUS_OK
            }
            Ok(Err(error)) => {
                // SAFETY: output is non-null and exclusively borrowed.
                unsafe { output.write(Buffer::from_vec(error.into_bytes())) };
                STATUS_FAILED
            }
            Err(_) => STATUS_PANICKED,
        }
    }

    unsafe extern "C" fn destroy_instance<P: NativePlugin>(instance: *mut c_void) {
        if !instance.is_null() {
            let _ = catch_unwind(AssertUnwindSafe(|| {
                // SAFETY: create returned this allocation; host destroys once.
                drop(unsafe { Box::from_raw(instance.cast::<P::Instance>()) });
            }));
        }
    }

    unsafe extern "C" fn destroy_factory<P: NativePlugin>(factory: *mut c_void) {
        if !factory.is_null() {
            let _ = catch_unwind(AssertUnwindSafe(|| {
                // SAFETY: plugin_api allocated this factory; host destroys once.
                drop(unsafe { Box::from_raw(factory.cast::<P>()) });
            }));
        }
    }

    unsafe extern "C" fn release_buffer(buffer: Buffer) {
        // SAFETY: This is the allocator-matched callback for exported buffers.
        unsafe { buffer.reclaim() };
    }

    let Ok(factory) = catch_unwind(AssertUnwindSafe(P::default)) else {
        return PluginApi::EMPTY;
    };
    PluginApi {
        abi_major: ABI_MAJOR,
        abi_minor: ABI_MINOR,
        struct_size: PluginApi::STRUCT_SIZE,
        reserved: 0,
        factory_handle: Box::into_raw(Box::new(factory)).cast(),
        descriptor: Some(descriptor::<P>),
        validate_config: Some(validate_config::<P>),
        create: Some(create::<P>),
        call: Some(call::<P>),
        destroy_instance: Some(destroy_instance::<P>),
        destroy_factory: Some(destroy_factory::<P>),
        release_buffer: Some(release_buffer),
    }
}
