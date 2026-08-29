use crate::{FrameHeader, RawBytes, STATUS_BUFFER_TOO_SMALL, STATUS_INVALID_ARGUMENT};
use core::ffi::c_void;

/// # Safety
///
/// `input` must be readable for `input_size` bytes for this synchronous call,
/// and its leading bytes must contain a valid, properly aligned `T` value.
pub(super) unsafe fn read_input<T: Copy>(input: *const c_void, input_size: u32) -> Result<T, u32> {
    if input.is_null()
        || input_size < size_u32::<T>()
        || !input.addr().is_multiple_of(align_of::<T>())
    {
        return Err(STATUS_INVALID_ARGUMENT);
    }
    // SAFETY: The caller supplied a non-null, aligned range with a complete T prefix.
    Ok(unsafe { input.cast::<T>().read() })
}

pub(super) fn validate_header(
    header: FrameHeader,
    minimum_size: u32,
    input_size: u32,
) -> Result<(), u32> {
    if header.is_compatible(minimum_size, input_size) {
        Ok(())
    } else {
        Err(STATUS_INVALID_ARGUMENT)
    }
}

pub(super) fn check_output<T>(output: *mut c_void, capacity: u32) -> Result<(), u32> {
    if output.is_null()
        || capacity < size_u32::<T>()
        || !output.addr().is_multiple_of(align_of::<T>())
    {
        Err(STATUS_BUFFER_TOO_SMALL)
    } else {
        Ok(())
    }
}

/// # Safety
///
/// `output` must be writable for `capacity` bytes for this synchronous call
/// and must not alias any live reference used while `value` is written.
pub(super) unsafe fn write_output<T>(
    output: *mut c_void,
    capacity: u32,
    value: T,
) -> Result<(), u32> {
    check_output::<T>(output, capacity)?;
    // SAFETY: The checked output range is aligned, writable, and large enough.
    unsafe { output.cast::<T>().write(value) };
    Ok(())
}

/// # Safety
///
/// A nonempty `raw` range must remain readable for its declared length for
/// this synchronous copy. The range may be borrowed and need not outlive it.
pub(super) unsafe fn copy_bytes(raw: RawBytes, maximum: usize) -> Result<Vec<u8>, u32> {
    let length = raw
        .checked_len(maximum)
        .map_err(|_| STATUS_INVALID_ARGUMENT)?;
    if length == 0 {
        return Ok(Vec::new());
    }
    // SAFETY: RawBytes validation established the non-null range. Native plugins
    // are trusted to honor readability for the synchronous exchange.
    Ok(unsafe { std::slice::from_raw_parts(raw.ptr, length) }.to_vec())
}

pub(super) fn size_u32<T>() -> u32 {
    u32::try_from(size_of::<T>()).expect("ABI frame type exceeds u32")
}
