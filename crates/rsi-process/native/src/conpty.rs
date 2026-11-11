//! Exact `ConPTY` handles. Policy, buffering and asynchronous supervision belong to callers.
use crate::{Job, launch::Attributes};
use std::{
    ffi::OsString,
    fs::File,
    io,
    os::windows::io::{AsHandle, AsRawHandle, FromRawHandle, OwnedHandle},
    path::Path,
    sync::Mutex,
};
use windows_sys::Win32::System::{
    Console::{COORD, ClosePseudoConsole, CreatePseudoConsole, ResizePseudoConsole},
    Threading::{
        CREATE_SUSPENDED, CREATE_UNICODE_ENVIRONMENT, CreateProcessW, EXTENDED_STARTUPINFO_PRESENT,
        INFINITE, PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE, PROCESS_INFORMATION, ResumeThread,
        STARTF_USESTDHANDLES, STARTUPINFOEXW, TerminateProcess, UpdateProcThreadAttribute,
        WaitForSingleObject,
    },
};

/// One console handle, serialized against resize and close.
#[derive(Debug)]
pub struct Console(Mutex<Option<isize>>);
impl Console {
    /// Creates a console using borrowed, non-inherited synchronous pipe endpoints.
    /// The caller must retain these endpoints until a client has been attached.
    pub fn new(input: &File, output: &File, rows: u16, columns: u16) -> io::Result<Self> {
        let size = dimensions(rows, columns)?;
        let mut handle = 0;
        // SAFETY: The borrowed pipe handles remain live for this call. The writable
        // handle slot is initialized; no cursor-inheritance handshake is requested.
        hresult(unsafe {
            CreatePseudoConsole(
                size,
                input.as_raw_handle(),
                output.as_raw_handle(),
                0,
                &raw mut handle,
            )
        })?;
        Ok(Self(Mutex::new(Some(handle))))
    }
    /// Starts an exact native executable suspended and assigns its Job before resume.
    pub fn spawn(
        &self,
        program: &Path,
        args: &[OsString],
        environment: &[(OsString, OsString)],
        cwd: &Path,
        job: &Job,
    ) -> io::Result<ConsoleChild> {
        if !program.is_absolute()
            || !cwd.is_absolute()
            || program
                .extension()
                .is_none_or(|v| !v.eq_ignore_ascii_case("exe"))
        {
            return Err(io::Error::other(
                "ConPTY requires an absolute native executable and directory",
            ));
        }
        let mut command = crate::launch::CommandLine::new(program, args, environment, cwd)?;
        let guard = self
            .0
            .lock()
            .map_err(|_| io::Error::other("console lock poisoned"))?;
        let handle = guard.ok_or_else(|| io::Error::other("console closed"))?;
        let mut attributes = Attributes::new()?;
        // SAFETY: The attribute buffer owns its initialized list and the console is
        // retained under its lock through CreateProcessW. HPCON is passed by value.
        if unsafe {
            UpdateProcThreadAttribute(
                attributes.pointer(),
                0,
                PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE as usize,
                handle as *const _,
                size_of::<isize>(),
                std::ptr::null_mut(),
                std::ptr::null(),
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        let mut startup = STARTUPINFOEXW::default();
        startup.StartupInfo.cb = u32::try_from(size_of::<STARTUPINFOEXW>())
            .map_err(|_| io::Error::other("Windows startup record exceeds its bound"))?;
        // ConPTY supplies these handles. Prevent a console parent (including a
        // test runner) from lending its own redirected standard streams.
        startup.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
        startup.StartupInfo.hStdInput = windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE;
        startup.StartupInfo.hStdOutput = windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE;
        startup.StartupInfo.hStdError = windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE;
        startup.lpAttributeList = attributes.pointer();
        let mut info = PROCESS_INFORMATION::default();
        // SAFETY: NUL-terminated UTF-16 inputs and the writable command line stay
        // live; the environment is double-NUL terminated. No handles are inherited.
        // CREATE_SUSPENDED prevents user code before Job assignment.
        if unsafe {
            CreateProcessW(
                command.application.as_ptr(),
                command.arguments.as_mut_ptr(),
                std::ptr::null(),
                std::ptr::null(),
                0,
                CREATE_SUSPENDED | CREATE_UNICODE_ENVIRONMENT | EXTENDED_STARTUPINFO_PRESENT,
                command.environment.as_ptr().cast(),
                command.directory.as_ptr(),
                &raw const startup.StartupInfo,
                &raw mut info,
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: Successful CreateProcessW transfers two distinct owned handles.
        let process = unsafe { OwnedHandle::from_raw_handle(info.hProcess) };
        // SAFETY: As above; this is the exact suspended primary thread.
        let thread = unsafe { OwnedHandle::from_raw_handle(info.hThread) };
        let child = ConsoleChild {
            process,
            pid: info.dwProcessId,
            status: None,
        };
        job.assign(child.process.as_raw_handle())?;
        // SAFETY: The retained primary thread belongs to this just-created child,
        // now assigned to the retained Job. It has not executed user code.
        if unsafe { ResumeThread(thread.as_raw_handle()) } != 1 {
            return Err(io::Error::last_os_error());
        }
        Ok(child)
    }
    /// Resizes a still-owned console.
    pub fn resize(&self, rows: u16, columns: u16) -> io::Result<()> {
        let size = dimensions(rows, columns)?;
        let guard = self
            .0
            .lock()
            .map_err(|_| io::Error::other("console lock poisoned"))?;
        let handle = guard.ok_or_else(|| io::Error::other("console closed"))?;
        // SAFETY: The mutex retains this live console against concurrent close.
        hresult(unsafe { ResizePseudoConsole(handle, size) })
    }
    /// Closes once. The caller must concurrently drain output to avoid pipe deadlock.
    pub fn close(&self) {
        let handle = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        if let Some(handle) = handle {
            // SAFETY: Taking the handle transfers its sole close obligation here.
            unsafe { ClosePseudoConsole(handle) };
        }
    }
}
impl Drop for Console {
    fn drop(&mut self) {
        self.close();
    }
}

/// Exact process owner; dropping before reap kills and waits for this client.
#[derive(Debug)]
pub struct ConsoleChild {
    process: OwnedHandle,
    pid: u32,
    status: Option<i32>,
}
impl ConsoleChild {
    /// Native child identity, retained by its handle.
    pub fn pid(&self) -> u32 {
        self.pid
    }
    /// Waits asynchronously and returns the direct child's Windows exit code.
    pub async fn wait(&mut self) -> io::Result<i32> {
        if let Some(status) = self.status {
            return Ok(status);
        }
        let status = crate::wait::wait(self.process.as_handle()).await?;
        self.status = Some(status);
        Ok(status)
    }
}
impl Drop for ConsoleChild {
    fn drop(&mut self) {
        if self.status.is_some() {
            return;
        }
        // SAFETY: This exact owned process cannot be replaced by PID reuse. Killing
        // an exited process is harmless; always wait before releasing the handle.
        unsafe {
            TerminateProcess(self.process.as_raw_handle(), 1);
            WaitForSingleObject(self.process.as_raw_handle(), INFINITE);
        }
    }
}
fn dimensions(rows: u16, columns: u16) -> io::Result<COORD> {
    if !(1..=200).contains(&rows) || !(1..=500).contains(&columns) {
        return Err(io::Error::other("invalid console dimensions"));
    }
    Ok(COORD {
        X: i16::try_from(columns).expect("validated columns"),
        Y: i16::try_from(rows).expect("validated rows"),
    })
}
fn hresult(result: i32) -> io::Result<()> {
    if result < 0 {
        Err(io::Error::other(format!(
            "ConPTY HRESULT 0x{:08x}",
            result.cast_unsigned()
        )))
    } else {
        Ok(())
    }
}
