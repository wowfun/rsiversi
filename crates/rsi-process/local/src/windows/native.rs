//! Narrow Job/thread syscalls. `OwnedHandle` is the only native handle owner.
use std::{
    ffi::c_void,
    io,
    os::windows::io::{AsRawHandle as _, FromRawHandle as _, OwnedHandle, RawHandle},
};

#[repr(C)]
#[derive(Default)]
struct Limits {
    process_time: i64,
    job_time: i64,
    flags: u32,
    minimum_working_set: usize,
    maximum_working_set: usize,
    active_limit: u32,
    affinity: usize,
    priority: u32,
    scheduling: u32,
}
#[repr(C)]
#[derive(Default)]
struct ExtendedLimits {
    basic: Limits,
    io: [u64; 6],
    process_memory: usize,
    job_memory: usize,
    peak_process_memory: usize,
    peak_job_memory: usize,
}
#[repr(C)]
#[derive(Default)]
struct Accounting {
    times: [i64; 4],
    faults: u32,
    total: u32,
    active: u32,
    terminated: u32,
}
#[repr(C)]
#[derive(Default)]
struct ThreadEntry {
    size: u32,
    usage: u32,
    id: u32,
    owner: u32,
    priority: i32,
    delta: i32,
    flags: u32,
}

#[link(name = "kernel32")]
unsafe extern "system" {
    fn CreateJobObjectW(security: *const c_void, name: *const u16) -> RawHandle;
    fn SetInformationJobObject(job: RawHandle, kind: u32, data: *const c_void, size: u32) -> i32;
    fn AssignProcessToJobObject(job: RawHandle, process: RawHandle) -> i32;
    fn TerminateJobObject(job: RawHandle, code: u32) -> i32;
    fn QueryInformationJobObject(
        job: RawHandle,
        kind: u32,
        data: *mut c_void,
        size: u32,
        returned: *mut u32,
    ) -> i32;
    fn CreateToolhelp32Snapshot(flags: u32, pid: u32) -> RawHandle;
    fn Thread32First(snapshot: RawHandle, entry: *mut ThreadEntry) -> i32;
    fn Thread32Next(snapshot: RawHandle, entry: *mut ThreadEntry) -> i32;
    fn OpenThread(access: u32, inherit: i32, id: u32) -> RawHandle;
    fn GetProcessIdOfThread(thread: RawHandle) -> u32;
    fn ResumeThread(thread: RawHandle) -> u32;
}

#[derive(Debug)]
pub(super) struct Job(OwnedHandle);
impl Job {
    pub(super) fn new() -> io::Result<Self> {
        // SAFETY: Null name/security create an unnamed, non-inheritable job handle.
        let handle = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
        if handle.is_null() {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: Successful creation transfers one new owned handle exactly once.
        let job = Self(unsafe { OwnedHandle::from_raw_handle(handle) });
        let limits = ExtendedLimits {
            basic: Limits {
                flags: 0x2000,
                ..Limits::default()
            },
            ..ExtendedLimits::default()
        };
        // SAFETY: The owned job and correctly aligned extended limits are live;
        // KILL_ON_JOB_CLOSE is the only flag, so no breakaway policy is enabled.
        if unsafe {
            SetInformationJobObject(
                job.0.as_raw_handle(),
                9,
                (&raw const limits).cast(),
                u32::try_from(std::mem::size_of::<ExtendedLimits>())
                    .expect("fixed Win32 record size"),
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        Ok(job)
    }
    pub(super) fn assign(&self, process: RawHandle) -> io::Result<()> {
        // SAFETY: The caller retains the suspended Child's process handle through
        // this call. Assignment occurs before resuming its user-code thread.
        if unsafe { AssignProcessToJobObject(self.0.as_raw_handle(), process) } == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }
    pub(super) fn terminate(&self) -> io::Result<()> {
        // SAFETY: The job is owned and live; termination addresses this object,
        // never a numeric PID that could refer to a different process generation.
        if unsafe { TerminateJobObject(self.0.as_raw_handle(), 1) } == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }
    pub(super) fn active(&self) -> io::Result<u32> {
        let mut accounting = Accounting::default();
        // SAFETY: The job and the aligned writable accounting record remain live.
        if unsafe {
            QueryInformationJobObject(
                self.0.as_raw_handle(),
                1,
                (&raw mut accounting).cast(),
                u32::try_from(std::mem::size_of::<Accounting>()).expect("fixed Win32 record size"),
                std::ptr::null_mut(),
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        Ok(accounting.active)
    }
}

pub(super) fn resume_primary(pid: u32) -> io::Result<()> {
    // SAFETY: TH32CS_SNAPTHREAD requests a process-independent thread snapshot.
    let handle = unsafe { CreateToolhelp32Snapshot(4, 0) };
    if handle as isize == -1 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: Snapshot success transfers one valid handle to its sole owner.
    let snapshot = unsafe { OwnedHandle::from_raw_handle(handle) };
    let mut entry = ThreadEntry {
        size: u32::try_from(std::mem::size_of::<ThreadEntry>()).expect("fixed Win32 record size"),
        ..ThreadEntry::default()
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
        if entry.owner == pid {
            if found.is_some() {
                return Err(io::Error::other("suspended child has multiple threads"));
            }
            // SAFETY: OpenThread returns a non-inheritable handle for the observed
            // ID. Its owner is checked again before any resume operation.
            let handle = unsafe { OpenThread(0x0802, 0, entry.id) };
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
        entry.size =
            u32::try_from(std::mem::size_of::<ThreadEntry>()).expect("fixed Win32 record size");
        // SAFETY: Same owned snapshot and fully initialized writable record.
        available = unsafe { Thread32Next(snapshot.as_raw_handle(), &raw mut entry) } != 0;
        if !available {
            let error = io::Error::last_os_error();
            if error.raw_os_error() != Some(18) {
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
