use std::{io, os::windows::io::AsRawHandle as _};
use windows_sys::Win32::Foundation::{
    ERROR_INVALID_HANDLE, HANDLE_FLAG_INHERIT, INVALID_HANDLE_VALUE, SetHandleInformation,
};

/// Prevents future children retaining this launcher's original standard pipes.
/// Explicit child stdio remains selected and duplicated by the spawning caller.
pub fn clear_standard_handle_inheritance() -> io::Result<()> {
    for handle in [
        io::stdin().as_raw_handle(),
        io::stdout().as_raw_handle(),
        io::stderr().as_raw_handle(),
    ] {
        if handle.is_null() || handle == INVALID_HANDLE_VALUE {
            continue;
        }
        // SAFETY: these borrowed standard handles remain owned by the process.
        // Clearing inheritance neither closes them nor expands their access.
        if unsafe { SetHandleInformation(handle, HANDLE_FLAG_INHERIT, 0) } == 0 {
            let error = io::Error::last_os_error();
            if error.raw_os_error() != Some(ERROR_INVALID_HANDLE.cast_signed()) {
                return Err(error);
            }
        }
    }
    Ok(())
}
