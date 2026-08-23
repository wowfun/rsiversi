use super::{Buffer, HostApi, STATUS_OK, copy_buffer};

#[derive(Clone, Copy)]
pub struct Host<'a> {
    api: &'a HostApi,
}

impl std::fmt::Debug for Host<'_> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.debug_struct("Host").finish_non_exhaustive()
    }
}

impl<'a> Host<'a> {
    pub(super) const fn new(api: &'a HostApi) -> Self {
        Self { api }
    }

    /// Calls one service declared as a requirement by this plugin.
    pub fn call(&self, service: &str, request: &[u8]) -> Result<Vec<u8>, String> {
        if !self.api.is_compatible() {
            return Err("incompatible host API".to_owned());
        }
        let mut output = Buffer::EMPTY;
        // SAFETY: The callback was validated; input borrows and output storage
        // remain valid for this synchronous invocation.
        let status = unsafe {
            self.api.call_service.expect("validated callback")(
                self.api.host_handle,
                service.as_ptr(),
                service.len(),
                request.as_ptr(),
                request.len(),
                &raw mut output,
            )
        };
        let bytes = if output.len > output.capacity {
            Err("host output length exceeds its capacity".to_owned())
        } else if output.ptr.is_null() && (output.len != 0 || output.capacity != 0) {
            Err("host output metadata has a null pointer".to_owned())
        } else {
            // SAFETY: Structural checks establish a non-null pointer for a
            // nonempty range. The host owns it until the release callback.
            Ok(unsafe { copy_buffer(output) })
        };
        // SAFETY: This is the allocator-matched callback and is called once.
        unsafe { self.api.release_buffer.expect("validated callback")(output) };
        let bytes = bytes?;
        if status == STATUS_OK {
            Ok(bytes)
        } else {
            Err(String::from_utf8_lossy(&bytes).into_owned())
        }
    }
}
