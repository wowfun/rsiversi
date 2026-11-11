//! Owned Windows allocations and token records; no workspace policy.
use std::{
    ffi::{OsStr, c_void},
    io,
    os::windows::{
        ffi::OsStrExt,
        io::{AsRawHandle, OwnedHandle},
    },
};
use windows_sys::Win32::{
    Foundation::LocalFree,
    Security::{
        Authorization::ConvertStringSidToSidW, GetTokenInformation, TOKEN_INFORMATION_CLASS,
    },
};

#[derive(Debug)]
pub(super) struct Local(pub(super) *mut c_void);
impl Drop for Local {
    fn drop(&mut self) {
        // SAFETY: Each Local owns one allocation from a documented LocalFree API.
        unsafe {
            LocalFree(self.0);
        }
    }
}
pub(super) fn failure(api: &str) -> io::Error {
    io::Error::other(format!("{api}: {}", io::Error::last_os_error()))
}
pub(super) fn check(value: i32, api: &str) -> io::Result<()> {
    if value == 0 {
        Err(failure(api))
    } else {
        Ok(())
    }
}
pub(super) fn status(value: u32, api: &str) -> io::Result<()> {
    if value == 0 {
        Ok(())
    } else {
        Err(io::Error::other(format!(
            "{api}: {}",
            io::Error::from_raw_os_error(value.cast_signed())
        )))
    }
}
pub(super) fn wide(value: &OsStr) -> io::Result<Vec<u16>> {
    let mut result: Vec<_> = value.encode_wide().collect();
    if result.contains(&0) {
        return Err(io::Error::other("NUL in native security path"));
    }
    result.push(0);
    Ok(result)
}
pub(super) fn sid(value: &str) -> io::Result<Local> {
    let text = wide(OsStr::new(value))?;
    let mut result = std::ptr::null_mut();
    // SAFETY: The terminated input and writable pointer slot stay live. Success
    // returns one complete SID allocated for LocalFree, owned exactly once here.
    check(
        unsafe { ConvertStringSidToSidW(text.as_ptr(), &raw mut result) },
        "ConvertStringSidToSidW",
    )?;
    Ok(Local(result))
}
pub(super) fn information(
    token: &OwnedHandle,
    class: TOKEN_INFORMATION_CLASS,
) -> io::Result<Vec<usize>> {
    let mut bytes = 0;
    // SAFETY: The sizing call only writes the required count for this live token.
    unsafe {
        GetTokenInformation(
            token.as_raw_handle(),
            class,
            std::ptr::null_mut(),
            0,
            &raw mut bytes,
        );
    }
    if bytes == 0 {
        return Err(failure("GetTokenInformation(size)"));
    }
    let mut data = vec![0usize; (bytes as usize).div_ceil(size_of::<usize>())];
    // SAFETY: The allocation is pointer-aligned and has at least bytes writable bytes.
    check(
        unsafe {
            GetTokenInformation(
                token.as_raw_handle(),
                class,
                data.as_mut_ptr().cast(),
                bytes,
                &raw mut bytes,
            )
        },
        "GetTokenInformation",
    )?;
    Ok(data)
}
