//! Narrow Job/thread syscalls. `OwnedHandle` is the only native handle owner.
use std::{
    io,
    os::windows::io::{AsRawHandle as _, FromRawHandle as _, OwnedHandle, RawHandle},
};
use windows_sys::Win32::{
    Foundation::{ERROR_NO_MORE_FILES, INVALID_HANDLE_VALUE},
    System::{
        Diagnostics::ToolHelp::{
            CreateToolhelp32Snapshot, TH32CS_SNAPTHREAD, THREADENTRY32, Thread32First, Thread32Next,
        },
        JobObjects::{
            AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
            JOBOBJECT_BASIC_ACCOUNTING_INFORMATION, JOBOBJECT_BASIC_LIMIT_INFORMATION,
            JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectBasicAccountingInformation,
            JobObjectExtendedLimitInformation, QueryInformationJobObject, SetInformationJobObject,
            TerminateJobObject,
        },
        Threading::{
            GetProcessIdOfThread, OpenThread, ResumeThread, THREAD_QUERY_LIMITED_INFORMATION,
            THREAD_SUSPEND_RESUME,
        },
    },
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
    /// Assigns and resumes a still-suspended synchronous child under its retained handle.
    pub fn admit_child(&self, child: &std::process::Child) -> io::Result<()> {
        self.assign(child.as_raw_handle())?;
        resume_primary(child.id())
    }
    /// Assigns and resumes a still-suspended asynchronous child under its retained handle.
    pub fn admit_tokio_child(&self, child: &tokio::process::Child) -> io::Result<()> {
        let handle = child
            .raw_handle()
            .ok_or_else(|| io::Error::other("child handle absent"))?;
        self.assign(handle)?;
        resume_primary(
            child
                .id()
                .ok_or_else(|| io::Error::other("child PID absent"))?,
        )
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

fn resume_primary(pid: u32) -> io::Result<()> {
    // SAFETY: TH32CS_SNAPTHREAD requests a process-independent thread snapshot.
    let handle = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0) };
    if handle == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: Snapshot success transfers one valid handle to its sole owner.
    let snapshot = unsafe { OwnedHandle::from_raw_handle(handle) };
    let mut entry = THREADENTRY32 {
        dwSize: u32::try_from(std::mem::size_of::<THREADENTRY32>())
            .expect("fixed Win32 record size"),
        ..THREADENTRY32::default()
    };
    let mut found = None;
    // SAFETY: The owned snapshot and initialized writable record are live.
    let mut available = unsafe { Thread32First(snapshot.as_raw_handle(), &raw mut entry) } != 0;
    let mut visited = 0;
    while available {
        visited += 1;
        if visited > 65536 {
            return Err(io::Error::other("thread snapshot exceeds its bound"));
        }
        if entry.th32OwnerProcessID == pid {
            if found.is_some() {
                return Err(io::Error::other("suspended child has multiple threads"));
            }
            // SAFETY: OpenThread returns a non-inheritable handle for the observed
            // ID. Its owner is checked again before any resume operation.
            let handle = unsafe {
                OpenThread(
                    THREAD_QUERY_LIMITED_INFORMATION | THREAD_SUSPEND_RESUME,
                    0,
                    entry.th32ThreadID,
                )
            };
            if handle.is_null() {
                return Err(io::Error::last_os_error());
            }
            // SAFETY: OpenThread success transfers one handle to this owner.
            let thread = unsafe { OwnedHandle::from_raw_handle(handle) };
            // SAFETY: The owned handle remains live; the Child's retained process
            // handle prevents PID reuse while this identity comparison is made.
            if unsafe { GetProcessIdOfThread(thread.as_raw_handle()) } != pid {
                return Err(io::Error::other("primary thread owner changed"));
            }
            found = Some(thread);
        }
        entry.dwSize =
            u32::try_from(std::mem::size_of::<THREADENTRY32>()).expect("fixed Win32 record size");
        // SAFETY: Same owned snapshot and fully initialized writable record.
        available = unsafe { Thread32Next(snapshot.as_raw_handle(), &raw mut entry) } != 0;
        if !available {
            let error = io::Error::last_os_error();
            if error.raw_os_error() != Some(ERROR_NO_MORE_FILES.cast_signed()) {
                return Err(error);
            }
        }
    }
    let thread = found.ok_or_else(|| io::Error::other("suspended primary thread is absent"))?;
    // SAFETY: The exact Child-owned thread was validated above and the caller
    // has assigned its still-suspended process to the managed Job Object.
    let previous = unsafe { ResumeThread(thread.as_raw_handle()) };
    if previous != 1 {
        return Err(io::Error::other(
            "primary thread was not suspended exactly once",
        ));
    }
    Ok(())
}
