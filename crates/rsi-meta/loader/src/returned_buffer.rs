use super::LoaderError;
use rsi_meta_plugin::{Buffer, ReleaseBufferFn, copy_buffer};

pub(super) struct ReturnedPluginBuffer {
    buffer: Option<Buffer>,
    release: ReleaseBufferFn,
}

impl ReturnedPluginBuffer {
    pub(super) fn new(buffer: Buffer, release: ReleaseBufferFn) -> Self {
        Self {
            buffer: Some(buffer),
            release,
        }
    }

    pub(super) fn copy(
        &self,
        maximum: usize,
        operation: &'static str,
    ) -> Result<Vec<u8>, LoaderError> {
        let buffer = self.buffer.expect("returned buffer remains owned");
        if buffer.len > maximum {
            return Err(LoaderError::Callback {
                operation,
                message: format!("output exceeded {maximum} bytes"),
            });
        }
        if buffer.len > buffer.capacity {
            return Err(LoaderError::Callback {
                operation,
                message: "output length exceeds its capacity".to_owned(),
            });
        }
        if buffer.ptr.is_null() && (buffer.len != 0 || buffer.capacity != 0) {
            return Err(LoaderError::Callback {
                operation,
                message: "nonempty output metadata has a null pointer".to_owned(),
            });
        }
        // SAFETY: Structural checks above establish a non-null pointer for a
        // nonempty range. The trusted plugin owns readable bytes until release.
        Ok(unsafe { copy_buffer(buffer) })
    }
}

impl Drop for ReturnedPluginBuffer {
    fn drop(&mut self) {
        let buffer = self.buffer.take().expect("returned buffer releases once");
        // SAFETY: The validated allocator callback owns this exact returned
        // buffer, and the Option gives Drop its unique release invocation.
        unsafe { (self.release)(buffer) };
    }
}
