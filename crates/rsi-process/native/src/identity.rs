use std::{
    io,
    os::windows::io::{AsRawHandle as _, FromRawHandle as _, OwnedHandle},
};
use windows_sys::Win32::{
    Foundation::{FILETIME, HANDLE, LocalFree, WAIT_FAILED, WAIT_TIMEOUT},
    Security::{
        Authorization::ConvertSidToStringSidW, GetTokenInformation, TOKEN_QUERY, TOKEN_USER,
        TokenUser,
    },
    System::Threading::{
        GetProcessTimes, OpenProcess, OpenProcessToken, PROCESS_QUERY_LIMITED_INFORMATION,
        PROCESS_SYNCHRONIZE, PROCESS_TERMINATE, TerminateProcess, WaitForSingleObject,
    },
};

/// Observed Windows process generation, independent from executable build identity.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProcessIdentity {
    /// Operating-system PID.
    pub pid: u32,
    /// Creation FILETIME represented as a decimal integer.
    pub start_token: String,
    /// Canonical `TokenUser` SID.
    pub user_sid: String,
}

/// Retained native handle; PID reuse cannot redirect checks or termination.
#[derive(Debug)]
pub struct ProcessHandle {
    handle: OwnedHandle,
    identity: ProcessIdentity,
}
impl ProcessHandle {
    /// Acquires query and synchronization rights to a live or exited process object.
    pub fn open(pid: u32) -> io::Result<Self> {
        Self::open_with(pid, false)
    }

    /// Acquires termination rights and compares the exact observed generation before returning.
    pub fn for_termination(expected: &ProcessIdentity) -> io::Result<Self> {
        let handle = Self::open_with(expected.pid, true)?;
        if handle.identity != *expected {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "process generation differs",
            ));
        }
        Ok(handle)
    }

    fn open_with(pid: u32, terminate: bool) -> io::Result<Self> {
        let access = PROCESS_QUERY_LIMITED_INFORMATION
            | PROCESS_SYNCHRONIZE
            | if terminate { PROCESS_TERMINATE } else { 0 };
        // SAFETY: OpenProcess has scalar inputs and returns a new non-inheritable handle.
        let handle = unsafe { OpenProcess(access, 0, pid) };
        if handle.is_null() {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: Successful OpenProcess transferred this valid owned kernel handle.
        let handle = unsafe { OwnedHandle::from_raw_handle(handle) };
        let mut creation = FILETIME::default();
        let mut exit = FILETIME::default();
        let mut kernel = FILETIME::default();
        let mut user = FILETIME::default();
        // SAFETY: The retained process and all FILETIME output storage remain live.
        if unsafe {
            GetProcessTimes(
                handle.as_raw_handle(),
                &raw mut creation,
                &raw mut exit,
                &raw mut kernel,
                &raw mut user,
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        let token = (u64::from(creation.dwHighDateTime) << 32) | u64::from(creation.dwLowDateTime);
        let identity = ProcessIdentity {
            pid,
            start_token: token.to_string(),
            user_sid: user_sid(handle.as_raw_handle())?,
        };
        Ok(Self { handle, identity })
    }

    /// Reads the identity captured from this same handle.
    pub fn identity(&self) -> &ProcessIdentity {
        &self.identity
    }

    /// Tests this process object without waiting or consulting a reused PID.
    pub fn is_running(&self) -> io::Result<bool> {
        // SAFETY: The owned handle has SYNCHRONIZE rights and remains live through the call.
        match unsafe { WaitForSingleObject(self.handle.as_raw_handle(), 0) } {
            WAIT_FAILED => Err(io::Error::last_os_error()),
            WAIT_TIMEOUT => Ok(true),
            _ => Ok(false),
        }
    }

    /// Requests termination of this retained object; callers still own waiting and reaping.
    pub fn terminate(&self, exit_code: u32) -> io::Result<()> {
        // SAFETY: The owned handle is valid. Windows enforces PROCESS_TERMINATE rights.
        if unsafe { TerminateProcess(self.handle.as_raw_handle(), exit_code) } == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }
}

struct LocalString(*mut u16);
impl Drop for LocalString {
    fn drop(&mut self) {
        // SAFETY: ConvertSidToStringSidW allocated this non-null buffer with LocalAlloc.
        unsafe { LocalFree(self.0.cast()) };
    }
}

fn user_sid(process: HANDLE) -> io::Result<String> {
    let mut token = std::ptr::null_mut();
    // SAFETY: The borrowed process handle and writable output remain valid; TOKEN_QUERY is read-only.
    if unsafe { OpenProcessToken(process, TOKEN_QUERY, &raw mut token) } == 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: OpenProcessToken transferred a new kernel handle on success.
    let token = unsafe { OwnedHandle::from_raw_handle(token) };
    let mut needed = 0;
    // SAFETY: The documented size query accepts a null buffer with zero length.
    unsafe {
        GetTokenInformation(
            token.as_raw_handle(),
            TokenUser,
            std::ptr::null_mut(),
            0,
            &raw mut needed,
        )
    };
    let length = needed as usize;
    if !(std::mem::size_of::<TOKEN_USER>()..=64 * 1024).contains(&length) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid TokenUser length",
        ));
    }
    let mut storage = vec![0_usize; length.div_ceil(std::mem::size_of::<usize>())];
    // SAFETY: The aligned owned allocation covers the bounded byte length returned by Windows.
    if unsafe {
        GetTokenInformation(
            token.as_raw_handle(),
            TokenUser,
            storage.as_mut_ptr().cast(),
            needed,
            &raw mut needed,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: Successful TokenUser query initialized the aligned TOKEN_USER header and its SID.
    let sid = unsafe { (*storage.as_ptr().cast::<TOKEN_USER>()).User.Sid };
    let mut text = std::ptr::null_mut();
    // SAFETY: The queried SID remains in storage and text receives a LocalAlloc-owned UTF-16 string.
    if unsafe { ConvertSidToStringSidW(sid, &raw mut text) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let text = LocalString(text);
    // SAFETY: SID text generated by Windows is null-terminated; a SID has at most
    // 15 subauthorities, so its canonical representation is below 256 UTF-16 units.
    let mut units = Vec::new();
    for index in 0..256 {
        let unit = unsafe { *text.0.add(index) };
        if unit == 0 {
            return String::from_utf16(&units)
                .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "invalid SID text"));
        }
        units.push(unit);
    }
    Err(io::Error::new(
        io::ErrorKind::InvalidData,
        "SID text exceeds native bound",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn process_identity_is_stable_and_mismatched_generation_cannot_be_controlled() {
        let current = ProcessHandle::open(std::process::id()).unwrap();
        assert!(current.is_running().unwrap());
        assert_eq!(
            current.identity(),
            ProcessHandle::open(std::process::id()).unwrap().identity()
        );
        assert!(current.identity().user_sid.starts_with("S-1-"));
        let mut wrong = current.identity().clone();
        wrong.start_token.push('0');
        assert!(ProcessHandle::for_termination(&wrong).is_err());
    }
}
