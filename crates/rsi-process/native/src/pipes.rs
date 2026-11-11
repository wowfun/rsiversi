//! Suspended native children with IOCP parent pipes and exact primary threads.
use crate::{
    Job,
    launch::{Attributes, CommandLine},
    wait,
};
use std::{
    ffi::OsString,
    fs::{File, OpenOptions},
    io,
    os::windows::{
        io::{AsHandle, AsRawHandle, BorrowedHandle, FromRawHandle, OwnedHandle},
        process::ExitStatusExt,
    },
    path::Path,
    process::ExitStatus,
};
use tokio::net::windows::named_pipe::{NamedPipeServer, ServerOptions};
use windows_sys::Win32::{
    Foundation::{DUPLICATE_SAME_ACCESS, DuplicateHandle},
    System::Threading::{
        CREATE_NO_WINDOW, CREATE_SUSPENDED, CREATE_UNICODE_ENVIRONMENT, CreateProcessAsUserW,
        CreateProcessW, EXTENDED_STARTUPINFO_PRESENT, GetCurrentProcess, INFINITE,
        PROC_THREAD_ATTRIBUTE_HANDLE_LIST, PROCESS_INFORMATION, ResumeThread, STARTF_USESHOWWINDOW,
        STARTF_USESTDHANDLES, STARTUPINFOEXW, TerminateProcess, UpdateProcThreadAttribute,
        WaitForSingleObject,
    },
};

/// Parent-side direction of an overlapped pipe and its synchronous child endpoint.
#[derive(Clone, Copy, Debug)]
pub enum PipeDirection {
    /// The parent reads bytes written by its child.
    Read,
    /// The parent writes bytes read by its child.
    Write,
}

/// Creates a connected, local-only pair with an 8 KiB native buffer.
/// Both endpoints are non-inheritable; a launcher explicitly duplicates child endpoints.
pub async fn async_pipe(direction: PipeDirection) -> io::Result<(NamedPipeServer, File)> {
    let mut random = [0; 16];
    getrandom::fill(&mut random).map_err(|error| io::Error::other(error.to_string()))?;
    let name = format!(
        r"\\.\pipe\rsi-process-{}-{}",
        std::process::id(),
        hex::encode(random)
    );
    let read = matches!(direction, PipeDirection::Read);
    let server = ServerOptions::new()
        .first_pipe_instance(true)
        .reject_remote_clients(true)
        .access_inbound(read)
        .access_outbound(!read)
        .in_buffer_size(8192)
        .out_buffer_size(8192)
        .create(&name)?;
    let child = OpenOptions::new().read(!read).write(read).open(&name)?;
    server.connect().await?;
    Ok((server, child))
}

/// Exact native spawn inputs; the token's confinement policy belongs to its issuer.
#[derive(Debug)]
pub struct PipeSpawn<'a> {
    /// Absolute native executable path.
    pub program: &'a Path,
    /// Argument vector excluding argv[0].
    pub arguments: &'a [OsString],
    /// Complete child environment without ambient merging.
    pub environment: &'a [(OsString, OsString)],
    /// Absolute child working directory.
    pub cwd: &'a Path,
    /// Optional borrowed primary token for `CreateProcessAsUserW`.
    pub token: Option<BorrowedHandle<'a>>,
}

/// A child created suspended until its exact primary thread is admitted to a Job.
/// Callers drain the pipes concurrently and wait before releasing Job admission.
#[derive(Debug)]
pub struct PipeChild {
    process: OwnedHandle,
    primary: Option<OwnedHandle>,
    pid: u32,
    status: Option<ExitStatus>,
    /// Parent write endpoint; dropping it supplies stdin EOF.
    pub stdin: Option<NamedPipeServer>,
    /// Parent read endpoint for stdout.
    pub stdout: Option<NamedPipeServer>,
    /// Parent read endpoint for stderr.
    pub stderr: Option<NamedPipeServer>,
}

impl PipeChild {
    /// Creates a hidden suspended child; it cannot execute before `resume` succeeds.
    pub async fn spawn(request: PipeSpawn<'_>) -> io::Result<Self> {
        if !request.program.is_absolute()
            || !request.cwd.is_absolute()
            || request.program.extension().is_some_and(|extension| {
                extension.eq_ignore_ascii_case("cmd") || extension.eq_ignore_ascii_case("bat")
            })
        {
            return Err(io::Error::other(
                "native pipes require an absolute executable and directory",
            ));
        }
        let mut command = CommandLine::new(
            request.program,
            request.arguments,
            request.environment,
            request.cwd,
        )?;
        let (stdin, input) = async_pipe(PipeDirection::Write).await?;
        let (stdout, output) = async_pipe(PipeDirection::Read).await?;
        let (stderr, error) = async_pipe(PipeDirection::Read).await?;
        let handles = [inherit(&input)?, inherit(&output)?, inherit(&error)?];
        let raw = handles.each_ref().map(AsRawHandle::as_raw_handle);
        let mut attributes = Attributes::new()?;
        // SAFETY: The initialized attribute list and all three inheritable
        // duplicates are retained through process creation; no other handle is lent.
        if unsafe {
            UpdateProcThreadAttribute(
                attributes.pointer(),
                0,
                PROC_THREAD_ATTRIBUTE_HANDLE_LIST as usize,
                raw.as_ptr().cast(),
                size_of_val(&raw),
                std::ptr::null_mut(),
                std::ptr::null(),
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        let mut startup = STARTUPINFOEXW::default();
        #[expect(
            clippy::cast_possible_truncation,
            reason = "The fixed Win32 STARTUPINFOEXW record fits its DWORD size field."
        )]
        {
            startup.StartupInfo.cb = size_of::<STARTUPINFOEXW>() as u32;
        }
        startup.StartupInfo.dwFlags = STARTF_USESTDHANDLES | STARTF_USESHOWWINDOW;
        startup.StartupInfo.wShowWindow = 0;
        startup.StartupInfo.hStdInput = raw[0];
        startup.StartupInfo.hStdOutput = raw[1];
        startup.StartupInfo.hStdError = raw[2];
        startup.lpAttributeList = attributes.pointer();
        let mut info = PROCESS_INFORMATION::default();
        let flags = CREATE_SUSPENDED
            | CREATE_NO_WINDOW
            | CREATE_UNICODE_ENVIRONMENT
            | EXTENDED_STARTUPINFO_PRESENT;
        // SAFETY: All NUL-terminated UTF-16 values, writable command storage,
        // double-NUL environment and explicit handle list remain live. A borrowed
        // primary token, when selected, also remains live. The child is suspended.
        let created = unsafe {
            match request.token {
                Some(token) => CreateProcessAsUserW(
                    token.as_raw_handle(),
                    command.application.as_ptr(),
                    command.arguments.as_mut_ptr(),
                    std::ptr::null(),
                    std::ptr::null(),
                    1,
                    flags,
                    command.environment.as_ptr().cast(),
                    command.directory.as_ptr(),
                    &raw const startup.StartupInfo,
                    &raw mut info,
                ),
                None => CreateProcessW(
                    command.application.as_ptr(),
                    command.arguments.as_mut_ptr(),
                    std::ptr::null(),
                    std::ptr::null(),
                    1,
                    flags,
                    command.environment.as_ptr().cast(),
                    command.directory.as_ptr(),
                    &raw const startup.StartupInfo,
                    &raw mut info,
                ),
            }
        };
        if created == 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: Successful creation transfers two independent handles, each
        // wrapped exactly once. No fallible operation follows before ownership.
        let process = unsafe { OwnedHandle::from_raw_handle(info.hProcess) };
        // SAFETY: This is the independently returned suspended primary thread.
        let primary = unsafe { OwnedHandle::from_raw_handle(info.hThread) };
        Ok(Self {
            process,
            primary: Some(primary),
            pid: info.dwProcessId,
            status: None,
            stdin: Some(stdin),
            stdout: Some(stdout),
            stderr: Some(stderr),
        })
    }

    /// Retained direct-child process identifier.
    pub fn id(&self) -> u32 {
        self.pid
    }

    /// Assigns the Job, then resumes the exact returned primary thread once.
    /// On failure the caller still owns this child and must terminate and wait.
    pub fn resume(&mut self, job: &Job) -> io::Result<()> {
        let thread = self
            .primary
            .as_ref()
            .ok_or_else(|| io::Error::other("child already resumed"))?;
        job.assign(self.process.as_raw_handle())?;
        // SAFETY: The retained primary thread belongs to this just-created
        // suspended process, now assigned to the caller's still-owned Job.
        if unsafe { ResumeThread(thread.as_raw_handle()) } != 1 {
            return Err(io::Error::other(
                "primary thread was not suspended exactly once",
            ));
        }
        self.primary.take();
        Ok(())
    }

    /// Waits without occupying a Tokio blocking worker; cancelling this wait is safe.
    pub async fn wait(&mut self) -> io::Result<ExitStatus> {
        if let Some(status) = self.status {
            return Ok(status);
        }
        let status = wait::wait(self.process.as_handle()).await?;
        let status = ExitStatus::from_raw(status.cast_unsigned());
        self.status = Some(status);
        Ok(status)
    }

    /// Checks the exact child without waiting.
    pub fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
        if self.status.is_none() {
            self.status = wait::try_status(self.process.as_handle())?
                .map(|code| ExitStatus::from_raw(code.cast_unsigned()));
        }
        Ok(self.status)
    }

    /// Terminates and reaps this direct child; callers separately settle its Job.
    pub async fn kill(&mut self) -> io::Result<()> {
        if self.try_wait()?.is_some() {
            return Ok(());
        }
        // SAFETY: The owned handle identifies the exact child generation.
        if unsafe { TerminateProcess(self.process.as_raw_handle(), 1) } == 0 {
            return Err(io::Error::last_os_error());
        }
        self.wait().await.map(|_| ())
    }
}

impl Drop for PipeChild {
    fn drop(&mut self) {
        if self.status.is_some() {
            return;
        }
        // SAFETY: This owner retains the exact process handle. Normal callers
        // have already reaped; a dropped suspended or failed child is killed and
        // reaped before releasing its last native ownership, as for ConsoleChild.
        unsafe {
            TerminateProcess(self.process.as_raw_handle(), 1);
            WaitForSingleObject(self.process.as_raw_handle(), INFINITE);
        }
    }
}

fn inherit(file: &File) -> io::Result<OwnedHandle> {
    let mut duplicate = std::ptr::null_mut();
    // SAFETY: The borrowed source stays live. Success returns a distinct owned,
    // inheritable duplicate for the explicit startup handle list only.
    if unsafe {
        DuplicateHandle(
            GetCurrentProcess(),
            file.as_raw_handle(),
            GetCurrentProcess(),
            &raw mut duplicate,
            0,
            1,
            DUPLICATE_SAME_ACCESS,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: DuplicateHandle transferred this new handle exactly once.
    Ok(unsafe { OwnedHandle::from_raw_handle(duplicate) })
}
