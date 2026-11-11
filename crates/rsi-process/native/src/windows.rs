//! Narrow Job/thread syscalls. `OwnedHandle` is the only native handle owner.
use std::{
    io,
    os::windows::io::{AsRawHandle as _, FromRawHandle as _, OwnedHandle, RawHandle},
};
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    JOBOBJECT_BASIC_ACCOUNTING_INFORMATION, JOBOBJECT_BASIC_LIMIT_INFORMATION,
    JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectBasicAccountingInformation,
    JobObjectExtendedLimitInformation, QueryInformationJobObject, SetInformationJobObject,
    TerminateJobObject,
};

/// Sole owner of an unnamed, non-inheritable Windows Job.
#[derive(Debug)]
pub struct Job(OwnedHandle);
impl Job {
    /// Creates kill-on-close ownership before any child starts.
    pub fn new() -> io::Result<Self> {
        // SAFETY: Null name/security create an unnamed, non-inheritable job handle.
        let handle = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
        if handle.is_null() {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: Successful creation transfers one new owned handle exactly once.
        let job = Self(unsafe { OwnedHandle::from_raw_handle(handle) });
        let limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION {
            BasicLimitInformation: JOBOBJECT_BASIC_LIMIT_INFORMATION {
                LimitFlags: JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
                ..JOBOBJECT_BASIC_LIMIT_INFORMATION::default()
            },
            ..JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default()
        };
        // SAFETY: The owned job and correctly aligned extended limits are live;
        // KILL_ON_JOB_CLOSE is the only flag, so no breakaway policy is enabled.
        if unsafe {
            SetInformationJobObject(
                job.0.as_raw_handle(),
                JobObjectExtendedLimitInformation,
                (&raw const limits).cast(),
                u32::try_from(std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>())
                    .map_err(|_| io::Error::other("native record exceeds Win32 size bound"))?,
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        Ok(job)
    }
    pub(crate) fn assign(&self, process: RawHandle) -> io::Result<()> {
        // SAFETY: The caller retains the suspended Child's process handle through
        // this call. Assignment occurs before resuming its user-code thread.
        if unsafe { AssignProcessToJobObject(self.0.as_raw_handle(), process) } == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }
    /// Terminates this exact Job and its members.
    pub fn terminate(&self) -> io::Result<()> {
        // SAFETY: The job is owned and live; termination addresses this object,
        // never a numeric PID that could refer to a different process generation.
        if unsafe { TerminateJobObject(self.0.as_raw_handle(), 1) } == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }
    /// Reports active processes without releasing Job ownership.
    pub fn active(&self) -> io::Result<u32> {
        let mut accounting = JOBOBJECT_BASIC_ACCOUNTING_INFORMATION::default();
        // SAFETY: The job and the aligned writable accounting record remain live.
        if unsafe {
            QueryInformationJobObject(
                self.0.as_raw_handle(),
                JobObjectBasicAccountingInformation,
                (&raw mut accounting).cast(),
                u32::try_from(std::mem::size_of::<JOBOBJECT_BASIC_ACCOUNTING_INFORMATION>())
                    .map_err(|_| io::Error::other("native record exceeds Win32 size bound"))?,
                std::ptr::null_mut(),
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        Ok(accounting.ActiveProcesses)
    }
}
