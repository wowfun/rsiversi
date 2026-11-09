//! Shared exact Win32 command encoding and process attribute storage.
use std::{
    ffi::{OsStr, OsString},
    io,
    os::windows::ffi::{OsStrExt, OsStringExt},
    path::Path,
};
use windows_sys::Win32::System::Threading::{
    DeleteProcThreadAttributeList, InitializeProcThreadAttributeList, LPPROC_THREAD_ATTRIBUTE_LIST,
};

pub(crate) struct CommandLine {
    pub(crate) application: Vec<u16>,
    pub(crate) arguments: Vec<u16>,
    pub(crate) directory: Vec<u16>,
    pub(crate) environment: Vec<u16>,
}
impl CommandLine {
    pub(crate) fn new(
        program: &Path,
        args: &[OsString],
        environment: &[(OsString, OsString)],
        cwd: &Path,
    ) -> io::Result<Self> {
        let program = win32_path(program)?;
        let application = wide(&program)?;
        let directory = wide(&win32_path(cwd)?)?;
        let mut command = vec![34];
        command.extend_from_slice(&application[..application.len() - 1]);
        command.push(34);
        for arg in args {
            command.push(32);
            quote(arg, &mut command)?;
        }
        command.push(0);
        if command.len() > 32767 {
            return Err(io::Error::other(
                "native command line exceeds Windows limit",
            ));
        }
        let mut entries = environment.iter().collect::<Vec<_>>();
        entries.sort_by_cached_key(|(key, _)| key.to_string_lossy().to_uppercase());
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
        Ok(Self {
            application,
            arguments: command,
            directory,
            environment: block,
        })
    }
}

pub(crate) struct Attributes(Vec<usize>, bool);
impl Attributes {
    pub(crate) fn new() -> io::Result<Self> {
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
    pub(crate) fn pointer(&mut self) -> LPPROC_THREAD_ATTRIBUTE_LIST {
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
            "native path has no equivalent Win32 spelling",
        ));
    }
    Ok(candidate.into_os_string())
}
fn quote(value: &OsStr, output: &mut Vec<u16>) -> io::Result<()> {
    let units = wide(value)?;
    let quoted = units.len() == 1 || units.iter().any(|unit| matches!(unit, 9 | 32));
    if quoted {
        output.push(34);
    }
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
    output.extend(std::iter::repeat_n(
        92,
        if quoted { slashes * 2 } else { slashes },
    ));
    if quoted {
        output.push(34);
    }
    Ok(())
}
