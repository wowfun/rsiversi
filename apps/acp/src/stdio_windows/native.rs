//! Exact stdio duplication and cancellation through owned native thread handles.
use std::{
    fs::File,
    io,
    os::windows::io::{AsRawHandle as _, FromRawHandle as _, RawHandle},
};
#[link(name = "kernel32")]
unsafe extern "system" {
    fn GetStdHandle(kind: u32) -> RawHandle;
    fn GetCurrentProcess() -> RawHandle;
    fn DuplicateHandle(
        source: RawHandle,
        handle: RawHandle,
        target: RawHandle,
        duplicate: *mut RawHandle,
        access: u32,
        inherit: i32,
        options: u32,
    ) -> i32;
    fn GetFileType(file: RawHandle) -> u32;
    #[cfg(test)]
    fn GetNamedPipeHandleStateW(
        pipe: RawHandle,
        mode: *mut u32,
        instances: *mut u32,
        collection: *mut u32,
        timeout: *mut u32,
        user: *mut u16,
        size: u32,
    ) -> i32;
    fn CancelSynchronousIo(thread: RawHandle) -> i32;

}
pub(super) fn standard_handle(kind: i32) -> io::Result<File> {
    // SAFETY: Standard handle lookup and current-process pseudo handles are
    // borrowed; DuplicateHandle creates the sole non-inheritable new owner.
    let (handle, process) = unsafe { (GetStdHandle(kind.cast_unsigned()), GetCurrentProcess()) };
    if handle.is_null() || handle as isize == -1 {
        return Err(io::Error::other("standard handle unavailable"));
    }
    let mut duplicate = std::ptr::null_mut();
    // SAFETY: All handles are live for this synchronous duplication; output is
    // writable, SAME_ACCESS is requested, and inheritance is explicitly false.
    if unsafe { DuplicateHandle(process, handle, process, &raw mut duplicate, 0, 0, 2) } == 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: A successful duplication transfers exactly one new handle.
    Ok(unsafe { File::from_raw_handle(duplicate) })
}
pub(super) fn kind(file: &File) -> io::Result<u32> {
    // SAFETY: File owns the live handle for this synchronous query.
    let kind = unsafe { GetFileType(file.as_raw_handle()) };
    if kind == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(kind)
}
#[cfg(test)]
pub(super) fn pipe_mode(file: &File) -> io::Result<u32> {
    let mut mode = 0;
    // SAFETY: File owns a live pipe, scalar output is writable, unused outputs
    // are null and no username buffer is requested.
    if unsafe {
        GetNamedPipeHandleStateW(
            file.as_raw_handle(),
            &raw mut mode,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            0,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    Ok(mode)
}

#[derive(Debug)]
pub(super) struct ThreadControl(std::os::windows::io::OwnedHandle);
impl ThreadControl {
    pub(super) fn new(thread: &std::thread::JoinHandle<()>) -> io::Result<Self> {
        // SAFETY: The pseudo handle is borrowed. The join handle keeps this
        // exact native thread alive while SAME_ACCESS duplication is performed.
        let process = unsafe { GetCurrentProcess() };
        let mut duplicate = std::ptr::null_mut();
        // SAFETY: Live borrowed handles and writable output remain valid. The
        // duplicate is non-inheritable and cannot target a recycled thread ID.
        if unsafe {
            DuplicateHandle(
                process,
                thread.as_raw_handle(),
                process,
                &raw mut duplicate,
                0,
                0,
                2,
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: Success transfers exactly one new thread handle.
        Ok(Self(unsafe {
            std::os::windows::io::OwnedHandle::from_raw_handle(duplicate)
        }))
    }
    pub(super) fn cancel(&self) {
        // SAFETY: This owned handle names only the admitted I/O worker. A race
        // with entry into ReadFile/WriteFile is handled by repeated cancellation
        // until its actual thread join, never by a numeric thread ID.
        unsafe {
            CancelSynchronousIo(self.0.as_raw_handle());
        }
    }
}
