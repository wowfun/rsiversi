//! One borrowed process handle and one joined Windows wait registration.
use std::{
    ffi::c_void,
    io,
    os::windows::io::{AsRawHandle, BorrowedHandle},
};
use tokio::sync::oneshot;
use windows_sys::Win32::{
    Foundation::{HANDLE, INVALID_HANDLE_VALUE, WAIT_OBJECT_0, WAIT_TIMEOUT},
    System::Threading::{
        GetExitCodeProcess, INFINITE, RegisterWaitForSingleObject, UnregisterWaitEx,
        WT_EXECUTEINWAITTHREAD, WT_EXECUTEONLYONCE, WaitForSingleObject,
    },
};

pub(crate) fn try_status(process: BorrowedHandle<'_>) -> io::Result<Option<i32>> {
    // SAFETY: The borrowed process handle remains live for the nonblocking wait.
    match unsafe { WaitForSingleObject(process.as_raw_handle(), 0) } {
        WAIT_OBJECT_0 => exit_code(process).map(Some),
        WAIT_TIMEOUT => Ok(None),
        _ => Err(io::Error::last_os_error()),
    }
}

fn exit_code(process: BorrowedHandle<'_>) -> io::Result<i32> {
    let mut code = 0;
    // SAFETY: The signalled process is retained and code is a writable DWORD.
    if unsafe { GetExitCodeProcess(process.as_raw_handle(), &raw mut code) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(code.cast_signed())
}

pub(crate) async fn wait(process: BorrowedHandle<'_>) -> io::Result<i32> {
    if let Some(code) = try_status(process)? {
        return Ok(code);
    }
    let mut registration = Registration::new(process)?;
    (&mut registration.receiver)
        .await
        .map_err(|_| io::Error::other("native process wait lost its notification"))?;
    exit_code(process)
}

struct Registration<'a> {
    receiver: oneshot::Receiver<()>,
    wait: HANDLE,
    context: *mut Option<oneshot::Sender<()>>,
    _process: BorrowedHandle<'a>,
}
// SAFETY: Only the one-shot OS callback accesses context until Drop joins it.
// The borrowed handle and notification channel are Send; no raw pointer escapes.
unsafe impl Send for Registration<'_> {}

impl<'a> Registration<'a> {
    fn new(process: BorrowedHandle<'a>) -> io::Result<Self> {
        let (sender, receiver) = oneshot::channel();
        let context = Box::into_raw(Box::new(Some(sender)));
        let mut wait = std::ptr::null_mut();
        // SAFETY: The registration borrows the process through unregistration.
        // context has a stable allocation; the sole callback sends once and does
        // not block. INFINITE excludes timer callbacks. Failure registers nothing.
        if unsafe {
            RegisterWaitForSingleObject(
                &raw mut wait,
                process.as_raw_handle(),
                Some(signalled),
                context.cast(),
                INFINITE,
                WT_EXECUTEONLYONCE | WT_EXECUTEINWAITTHREAD,
            )
        } == 0
        {
            let error = io::Error::last_os_error();
            // SAFETY: Failed registration transferred no callback ownership.
            drop(unsafe { Box::from_raw(context) });
            return Err(error);
        }
        Ok(Self {
            receiver,
            wait,
            context,
            _process: process,
        })
    }
}

impl Drop for Registration<'_> {
    fn drop(&mut self) {
        // SAFETY: This is the sole live registration owner. The sentinel joins
        // any callback before its allocation or borrowed process can be released.
        // This owner is dropped by the awaiting task, never by the OS callback.
        let joined = unsafe { UnregisterWaitEx(self.wait, INVALID_HANDLE_VALUE) };
        assert_ne!(
            joined,
            0,
            "cannot join native process wait: {}",
            io::Error::last_os_error()
        );
        // SAFETY: Successful unregistration joined the only possible callback.
        drop(unsafe { Box::from_raw(self.context) });
    }
}

unsafe extern "system" fn signalled(context: *mut c_void, _timed_out: bool) {
    // SAFETY: Registration owns this allocation until the one-shot callback has
    // returned. No other thread accesses the sender while the callback runs.
    let sender = unsafe { &mut *context.cast::<Option<oneshot::Sender<()>>>() };
    if let Some(sender) = sender.take() {
        let _ = sender.send(());
    }
}
