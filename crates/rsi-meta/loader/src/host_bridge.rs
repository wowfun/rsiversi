use super::worker::{CallbackCompletion, CompletionOnDrop};
use super::{LoaderError, NativeInstance};
use rsi_meta::{Context, ServiceFrame};
use rsi_meta_plugin::{ABI_MAJOR, ABI_MINOR, Buffer, HostApi, STATUS_OK, borrow_abi_input};
use std::ffi::c_void;
use std::sync::atomic::Ordering;

struct HostCallContext {
    runtime: tokio::runtime::Handle,
    context: Context,
    maximum_identifier_bytes: usize,
    maximum_frame_bytes: usize,
}

unsafe extern "C" fn host_call_service(
    handle: *mut c_void,
    service_ptr: *const u8,
    service_len: usize,
    request_ptr: *const u8,
    request_len: usize,
    output: *mut Buffer,
) -> u32 {
    if handle.is_null()
        || output.is_null()
        || (service_ptr.is_null() && service_len != 0)
        || (request_ptr.is_null() && request_len != 0)
    {
        return rsi_meta_plugin::STATUS_INVALID_ARGUMENT;
    }
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        // SAFETY: call_native owns this exact context for the callback duration.
        let host = unsafe { &*(handle.cast::<HostCallContext>()) };
        if service_len > host.maximum_identifier_bytes {
            return Err("outbound service identifier exceeds the host limit".to_owned());
        }
        if request_len > host.maximum_frame_bytes {
            return Err("outbound service request exceeds the host frame limit".to_owned());
        }
        // SAFETY: The caller borrows these pointers for this synchronous call.
        let service = unsafe { borrow_abi_input(service_ptr, service_len) };
        let service = std::str::from_utf8(service).map_err(|error| error.to_string())?;
        // SAFETY: The caller borrows these pointers for this synchronous call.
        let request = unsafe { borrow_abi_input(request_ptr, request_len) };
        host.runtime.block_on(async {
            host.context
                .service(service)
                .map_err(|error| error.to_string())?
                .open()
                .map_err(|error| error.to_string())?
                .unary(ServiceFrame::new(request.to_vec()))
                .await
                .map(ServiceFrame::into_bytes)
                .map_err(|error| error.to_string())
        })
    }));
    let (status, bytes) = match result {
        Ok(Ok(bytes)) => (STATUS_OK, bytes),
        Ok(Err(error)) => (rsi_meta_plugin::STATUS_FAILED, error.into_bytes()),
        Err(_) => (
            rsi_meta_plugin::STATUS_PANICKED,
            b"host service bridge panicked".to_vec(),
        ),
    };
    // SAFETY: output is non-null and exclusively borrowed for this call.
    unsafe { output.write(Buffer::from_vec(bytes)) };
    status
}

unsafe extern "C" fn release_host_buffer(buffer: Buffer) {
    // SAFETY: Host bridge buffers are allocated by Buffer::from_vec here.
    unsafe { buffer.reclaim() };
}

pub(super) fn call_native(
    instance: &NativeInstance,
    completed: &CallbackCompletion,
    runtime: tokio::runtime::Handle,
    context: Context,
    maximum_frame_bytes: usize,
    service: &str,
    request: &[u8],
) -> Result<Vec<u8>, LoaderError> {
    // Install panic completion before the poison check so every return path
    // publishes callback completion; no earlier duplicate load is needed.
    let _completion = CompletionOnDrop(completed);
    if instance.poisoned.load(Ordering::Acquire) {
        return Err(LoaderError::Callback {
            operation: "call",
            message: "instance was poisoned by a timed-out callback".to_owned(),
        });
    }
    let mut host_context = HostCallContext {
        runtime,
        maximum_identifier_bytes: context.runtime().limits().payloads.maximum_identifier_bytes,
        context,
        maximum_frame_bytes,
    };
    let host_api = HostApi {
        abi_major: ABI_MAJOR,
        abi_minor: ABI_MINOR,
        struct_size: HostApi::STRUCT_SIZE,
        reserved: 0,
        host_handle: (&raw mut host_context).cast(),
        call_service: Some(host_call_service),
        release_buffer: Some(release_host_buffer),
    };
    let mut output = Buffer::EMPTY;
    // SAFETY: Instance/module are live, host context and inputs are borrowed for
    // this synchronous call, and output is writable.
    let status = unsafe {
        instance.module.api.call.expect("validated API")(
            instance.handle,
            &raw const host_api,
            service.as_ptr(),
            service.len(),
            request.as_ptr(),
            request.len(),
            &raw mut output,
        )
    };
    instance
        .module
        .take_buffer(status, output, maximum_frame_bytes, "call")
}

#[cfg(test)]
mod tests {
    use super::*;
    use rsi_meta::Runtime;

    #[tokio::test(flavor = "current_thread")]
    async fn host_bridge_rejects_oversized_lengths_before_borrowing_plugin_memory() {
        let runtime = Runtime::default();
        let mut host = HostCallContext {
            runtime: tokio::runtime::Handle::current(),
            context: runtime.root(),
            maximum_identifier_bytes: 4,
            maximum_frame_bytes: 4,
        };
        let inaccessible = std::ptr::NonNull::<u8>::dangling().as_ptr();

        for (service_len, request_len) in [(5, 0), (0, 5)] {
            let mut output = Buffer::EMPTY;
            // SAFETY: Over-limit lengths are rejected before either pointer is
            // borrowed; the test deliberately uses an inaccessible non-null
            // address to make that ordering part of the callback contract.
            let status = unsafe {
                host_call_service(
                    (&raw mut host).cast(),
                    inaccessible,
                    service_len,
                    inaccessible,
                    request_len,
                    &raw mut output,
                )
            };
            assert_eq!(status, rsi_meta_plugin::STATUS_FAILED);
            // SAFETY: host_call_service initialized this host-owned buffer.
            unsafe { output.reclaim() };
        }
    }
}
