//! Exact `ConPTY` handles. Policy, buffering and asynchronous supervision belong to callers.
use crate::Job;
use std::{
    ffi::{OsStr, OsString},
    fs::File,
    io,
    os::windows::{
        ffi::{OsStrExt, OsStringExt},
        io::{AsRawHandle, FromRawHandle, OwnedHandle},
    },
    path::Path,
    sync::Mutex,
};
use windows_sys::Win32::{
    Foundation::WAIT_OBJECT_0,
    System::{
        Console::{COORD, ClosePseudoConsole, CreatePseudoConsole, ResizePseudoConsole},
        Threading::{
            CREATE_SUSPENDED, CREATE_UNICODE_ENVIRONMENT, CreateProcessW,
            DeleteProcThreadAttributeList, EXTENDED_STARTUPINFO_PRESENT, GetExitCodeProcess,
            INFINITE, InitializeProcThreadAttributeList, LPPROC_THREAD_ATTRIBUTE_LIST,
            PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE, PROCESS_INFORMATION, ResumeThread,
            STARTF_USESTDHANDLES, STARTUPINFOEXW, TerminateProcess, UpdateProcThreadAttribute,
            WaitForSingleObject,
        },
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
        let program = win32_path(program)?;
        let application = wide(&program)?;
        let directory = wide(&win32_path(cwd)?)?;
        let mut command = Vec::new();
        for arg in std::iter::once(program.as_os_str()).chain(args.iter().map(OsString::as_os_str))
        {
            if !command.is_empty() {
                command.push(32);
            }
            quote(arg, &mut command)?;
        }
        command.push(0);
        if command.len() > 32767 {
            return Err(io::Error::other(
                "ConPTY command line exceeds Windows limit",
            ));
        }
        let mut entries = environment.iter().collect::<Vec<_>>();
        entries.sort_by_key(|(key, _)| key.to_string_lossy().to_uppercase());
        let mut block = Vec::new();
        for (key, value) in entries {
            let key = wide(key)?;
            if key.len() < 2 || key.contains(&61) {
                return Err(io::Error::other("invalid environment key"));
            }
            block.extend_from_slice(&key[..key.len() - 1]);
            block.push(61);
            block.extend(wide(value)?);
        }
        if block.is_empty() {
            block.push(0);
        }
        block.push(0);
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
                application.as_ptr(),
                command.as_mut_ptr(),
                std::ptr::null(),
                std::ptr::null(),
                0,
                CREATE_SUSPENDED | CREATE_UNICODE_ENVIRONMENT | EXTENDED_STARTUPINFO_PRESENT,
                block.as_ptr().cast(),
                directory.as_ptr(),
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
}
impl ConsoleChild {
    /// Native child identity, retained by its handle.
    pub fn pid(&self) -> u32 {
        self.pid
    }
    /// Blocks until the direct child exits and returns its Windows exit code.
    pub fn wait(&self) -> io::Result<i32> {
        // SAFETY: The owned process handle remains valid throughout the wait.
        if unsafe { WaitForSingleObject(self.process.as_raw_handle(), INFINITE) } != WAIT_OBJECT_0 {
            return Err(io::Error::last_os_error());
        }
        let mut code = 0;
        // SAFETY: The signalled process is retained and code is a writable DWORD.
        if unsafe { GetExitCodeProcess(self.process.as_raw_handle(), &raw mut code) } == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(code.cast_signed())
    }
}
impl Drop for ConsoleChild {
    fn drop(&mut self) {
        // SAFETY: This exact owned process cannot be replaced by PID reuse. Killing
        // an exited process is harmless; always wait before releasing the handle.
        unsafe {
            TerminateProcess(self.process.as_raw_handle(), 1);
        }
        let _ = self.wait();
    }
}
struct Attributes(Vec<usize>, bool);
impl Attributes {
    fn new() -> io::Result<Self> {
        let mut bytes = 0;
        // SAFETY: The documented sizing call writes only the required byte count.
        unsafe {
            InitializeProcThreadAttributeList(std::ptr::null_mut(), 1, 0, &raw mut bytes);
        }
        if bytes == 0 || bytes > 65536 {
            return Err(io::Error::other("invalid process attribute size"));
        }
        let mut value = Self(vec![0; bytes.div_ceil(size_of::<usize>())], false);
        // SAFETY: The aligned allocation has at least the requested byte length.
        if unsafe { InitializeProcThreadAttributeList(value.pointer(), 1, 0, &raw mut bytes) } == 0
        {
            return Err(io::Error::last_os_error());
        }
        value.1 = true;
        Ok(value)
    }
    fn pointer(&mut self) -> LPPROC_THREAD_ATTRIBUTE_LIST {
        self.0.as_mut_ptr().cast()
    }
}
impl Drop for Attributes {
    fn drop(&mut self) {
        // SAFETY: This owner retains its initialized list until deletion.
        if self.1 {
            unsafe {
                DeleteProcThreadAttributeList(self.pointer());
            }
        }
    }
}
fn wide(value: &OsStr) -> io::Result<Vec<u16>> {
    let mut result = value.encode_wide().collect::<Vec<_>>();
    if result.contains(&0) {
        return Err(io::Error::other("NUL in native process input"));
    }
    result.push(0);
    Ok(result)
}
fn win32_path(path: &Path) -> io::Result<OsString> {
    let original = wide(path.as_os_str())?;
    // CreateProcess's current directory and .NET executable configuration need
    // Win32 spelling. Sandbox and Execution return verbatim paths. Strip only
    // after proving the normal spelling resolves to the exact same target;
    // reserved DOS names and trailing-dot/space paths must never be retargeted.
    let prefix = [92, 92, 63, 92];
    if !original.starts_with(&prefix) {
        return Ok(path.as_os_str().to_owned());
    }
    let candidate = if original[4..].starts_with(&[85, 78, 67, 92]) {
        [vec![92, 92], original[8..original.len() - 1].to_vec()].concat()
    } else {
        original[4..original.len() - 1].to_vec()
    };
    let candidate = std::path::PathBuf::from(OsString::from_wide(&candidate));
    if !candidate.is_absolute() || candidate.canonicalize()? != path.canonicalize()? {
        return Err(io::Error::other(
            "console path has no equivalent Win32 spelling",
        ));
    }
    Ok(candidate.into_os_string())
}
fn quote(value: &OsStr, output: &mut Vec<u16>) -> io::Result<()> {
    let units = wide(value)?;
    output.push(34);
    let mut slashes = 0;
    for &unit in &units[..units.len() - 1] {
        if unit == 92 {
            slashes += 1;
            continue;
        }
        output.extend(std::iter::repeat_n(
            92,
            if unit == 34 { slashes * 2 + 1 } else { slashes },
        ));
        output.push(unit);
        slashes = 0;
    }
    output.extend(std::iter::repeat_n(92, slashes * 2));
    output.push(34);
    Ok(())
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
