//! Atomic directory descriptor construction and verification through owned handles.
use super::Handle;
use std::{
    ffi::c_void,
    fs::File,
    io,
    os::windows::io::{AsRawHandle as _, FromRawHandle as _, OwnedHandle},
};

// ABI declarations match Win32 TOKEN_USER and self-relative security-descriptor
// APIs. Pointer lifetimes, output ownership and bounds are specified at each call.
#[link(name = "advapi32")]
unsafe extern "system" {
    fn OpenProcessToken(process: Handle, access: u32, token: *mut Handle) -> i32;
    fn GetTokenInformation(
        token: Handle,
        kind: u32,
        data: *mut c_void,
        size: u32,
        needed: *mut u32,
    ) -> i32;
    fn ConvertStringSecurityDescriptorToSecurityDescriptorW(
        text: *const u16,
        revision: u32,
        descriptor: *mut Handle,
        size: *mut u32,
    ) -> i32;
    fn ConvertSecurityDescriptorToStringSecurityDescriptorW(
        descriptor: Handle,
        revision: u32,
        information: u32,
        text: *mut *mut u16,
        size: *mut u32,
    ) -> i32;
    fn GetSecurityInfo(
        file: Handle,
        kind: u32,
        information: u32,
        owner: *mut Handle,
        group: *mut Handle,
        dacl: *mut Handle,
        sacl: *mut Handle,
        descriptor: *mut Handle,
    ) -> u32;
    fn GetSecurityDescriptorControl(
        descriptor: Handle,
        control: *mut u16,
        revision: *mut u32,
    ) -> i32;
    fn GetSecurityDescriptorOwner(
        descriptor: Handle,
        owner: *mut Handle,
        defaulted: *mut i32,
    ) -> i32;
    fn GetSecurityDescriptorDacl(
        descriptor: Handle,
        present: *mut i32,
        dacl: *mut Handle,
        defaulted: *mut i32,
    ) -> i32;
    fn EqualSid(first: Handle, second: Handle) -> i32;
    fn SetSecurityInfo(
        file: Handle,
        kind: u32,
        information: u32,
        owner: Handle,
        group: Handle,
        dacl: Handle,
        sacl: Handle,
    ) -> u32;
}
#[link(name = "kernel32")]
unsafe extern "system" {
    fn GetCurrentProcess() -> Handle;
    fn LocalFree(memory: Handle) -> Handle;
}

struct Allocation(Handle);
impl Drop for Allocation {
    fn drop(&mut self) {
        // SAFETY: This is exactly one live LocalAlloc allocation returned by a
        // successful descriptor API; no borrowed native view survives this owner.
        unsafe {
            LocalFree(self.0);
        }
    }
}

pub(super) struct PrivateDescriptor {
    allocation: Allocation,
    policy: String,
}

impl PrivateDescriptor {
    pub(super) fn new(directory: bool) -> io::Result<Self> {
        let user = token_user()?;
        Self::with_user(directory, &user)
    }

    fn with_user(directory: bool, user: &str) -> io::Result<Self> {
        let inherit = if directory { "OICI" } else { "" };
        let policy = format!(
            "O:{user}D:P(A;{inherit};FA;;;SY)(A;{inherit};FA;;;BA)(A;{inherit};FA;;;{user})"
        );
        let text: Vec<_> = policy.encode_utf16().chain([0]).collect();
        let mut descriptor = std::ptr::null_mut();
        let mut size = 0;
        // SAFETY: The NUL-terminated SDDL and outputs are live during this call.
        // On success the returned LocalAlloc block transfers to Allocation.
        let ok = unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                text.as_ptr(),
                1,
                &raw mut descriptor,
                &raw mut size,
            )
        };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        let allocation = Allocation(descriptor);
        let policy = descriptor_text(allocation.0)?;
        Ok(Self { allocation, policy })
    }

    pub(super) fn as_ptr(&self) -> *const c_void {
        self.allocation.0.cast_const()
    }

    pub(super) fn apply(&self, file: &File) -> io::Result<()> {
        let mut actual_owner = std::ptr::null_mut();
        let mut observed = std::ptr::null_mut();
        // SAFETY: The owned file has READ_CONTROL/WRITE_DAC; GetSecurityInfo's SID
        // view remains valid through its separately owned LocalAlloc descriptor.
        let error = unsafe {
            GetSecurityInfo(
                file.as_raw_handle(),
                1,
                1,
                &raw mut actual_owner,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                &raw mut observed,
            )
        };
        if error != 0 {
            return Err(io::Error::from_raw_os_error(error.cast_signed()));
        }
        let _observed = Allocation(observed);
        let mut expected_owner = std::ptr::null_mut();
        let mut defaulted = 0;
        let mut present = 0;
        let mut dacl = std::ptr::null_mut();
        // SAFETY: Both descriptors and all output scalars remain live. The policy
        // contains an explicit owner and DACL; no pointer escapes these owners.
        let valid = unsafe {
            GetSecurityDescriptorOwner(
                self.allocation.0,
                &raw mut expected_owner,
                &raw mut defaulted,
            ) != 0
                && !actual_owner.is_null()
                && !expected_owner.is_null()
                && EqualSid(actual_owner, expected_owner) != 0
                && GetSecurityDescriptorDacl(
                    self.allocation.0,
                    &raw mut present,
                    &raw mut dacl,
                    &raw mut defaulted,
                ) != 0
                && present != 0
                && !dacl.is_null()
        };
        if !valid {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "private directory owner differs from TokenUser",
            ));
        }
        // SAFETY: WRITE_DAC is held on the exact opened object. This replaces only
        // its DACL, protects it against inheritance, and never changes ownership.
        let error = unsafe {
            SetSecurityInfo(
                file.as_raw_handle(),
                1,
                0x8000_0004,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                dacl,
                std::ptr::null_mut(),
            )
        };
        if error != 0 {
            return Err(io::Error::from_raw_os_error(error.cast_signed()));
        }
        self.verify(file)
    }

    pub(super) fn verify(&self, file: &File) -> io::Result<()> {
        // SetSecurityInfo preserves SE_DACL_AUTO_INHERITED on an existing object.
        // With a protected DACL and identical explicit ACEs this historical marker
        // grants no additional authority; inherited ACEs are still compared exactly.
        let actual = observed(file)?.replace("D:PAI(", "D:P(");
        if actual != self.policy {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "private object's descriptor changed",
            ));
        }
        Ok(())
    }
}

fn token_user() -> io::Result<String> {
    let mut token = std::ptr::null_mut();
    // SAFETY: GetCurrentProcess returns a borrowed pseudo-handle. TOKEN_QUERY
    // and the live output acquire one owned token handle, never inherited.
    let ok = unsafe { OpenProcessToken(GetCurrentProcess(), 8, &raw mut token) };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: Successful OpenProcessToken transfers a new valid kernel handle.
    let token = unsafe { OwnedHandle::from_raw_handle(token) };
    let mut needed = 0;
    // SAFETY: Null data/zero size is the documented sizing query; needed is live.
    let ok = unsafe {
        GetTokenInformation(
            token.as_raw_handle(),
            1,
            std::ptr::null_mut(),
            0,
            &raw mut needed,
        )
    };
    if ok != 0
        || io::Error::last_os_error().raw_os_error() != Some(122)
        || !(16..=65536).contains(&needed)
    {
        return Err(io::Error::other("invalid TokenUser size query"));
    }
    let mut buffer = vec![0_u64; (needed as usize).div_ceil(8)];
    let capacity = needed;
    // SAFETY: buffer is aligned and zeroed, with capacity bytes available; the
    // token and output-size field stay live through this synchronous query.
    let ok = unsafe {
        GetTokenInformation(
            token.as_raw_handle(),
            1,
            buffer.as_mut_ptr().cast(),
            capacity,
            &raw mut needed,
        )
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    if needed < 16 || needed > capacity {
        return Err(invalid_sid());
    }
    // SAFETY: The validated TOKEN_USER record contains a properly aligned SID
    // pointer at offset zero. Reading the pointer does not dereference its target.
    let sid = unsafe { buffer.as_ptr().cast::<Handle>().read() };
    let offset = (sid as usize)
        .checked_sub(buffer.as_ptr() as usize)
        .ok_or_else(invalid_sid)?;
    let bytes: Vec<_> = buffer
        .into_iter()
        .flat_map(u64::to_ne_bytes)
        .take(needed as usize)
        .collect();
    let header = bytes
        .get(offset..offset.checked_add(8).ok_or_else(invalid_sid)?)
        .ok_or_else(invalid_sid)?;
    let count = usize::from(header[1]);
    if header[0] != 1 || count > 15 {
        return Err(invalid_sid());
    }
    let sub = bytes
        .get(offset + 8..offset + 8 + count * 4)
        .ok_or_else(invalid_sid)?;
    let authority = header[2..8]
        .iter()
        .fold(0_u64, |number, byte| (number << 8) | u64::from(*byte));
    let mut result = format!("S-1-{authority}");
    for part in sub.chunks_exact(4) {
        use std::fmt::Write as _;
        write!(
            &mut result,
            "-{}",
            u32::from_le_bytes(part.try_into().expect("SID subauthority"))
        )
        .expect("String write");
    }
    Ok(result)
}

fn invalid_sid() -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, "invalid TokenUser SID record")
}

pub(super) fn observed(file: &File) -> io::Result<String> {
    let mut descriptor = std::ptr::null_mut();
    // SAFETY: file is a live handle with READ_CONTROL; unused component outputs
    // are null. Successful GetSecurityInfo transfers one LocalAlloc descriptor.
    let error = unsafe {
        GetSecurityInfo(
            file.as_raw_handle(),
            1,
            5,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &raw mut descriptor,
        )
    };
    if error != 0 {
        return Err(io::Error::from_raw_os_error(error.cast_signed()));
    }
    let descriptor = Allocation(descriptor);
    let mut control = 0;
    let mut revision = 0;
    // SAFETY: descriptor remains owned and valid; both scalar outputs are live.
    let ok =
        unsafe { GetSecurityDescriptorControl(descriptor.0, &raw mut control, &raw mut revision) };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    if control & 0x1000 == 0 {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "private DACL is not protected",
        ));
    }
    descriptor_text(descriptor.0)
}

fn descriptor_text(descriptor: Handle) -> io::Result<String> {
    let mut text = std::ptr::null_mut();
    let mut wchar_count = 0;
    // SAFETY: The owned descriptor remains valid during conversion. Success
    // returns a NUL-terminated LocalAlloc string and its allocation size in WCHARs.
    let ok = unsafe {
        ConvertSecurityDescriptorToStringSecurityDescriptorW(
            descriptor,
            1,
            5,
            &raw mut text,
            &raw mut wchar_count,
        )
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    let _allocation = Allocation(text.cast());
    if !(1..=32768).contains(&wchar_count) {
        return Err(io::Error::other(
            "native descriptor string exceeds its bound",
        ));
    }
    let mut units = Vec::new();
    for index in 0..wchar_count as usize {
        // SAFETY: Conversion returns a NUL-terminated string within the reported
        // allocation bound. Read only through its first NUL, never reserved tail
        // capacity. The Allocation owner keeps the string alive throughout.
        let unit = unsafe { text.add(index).read() };
        if unit == 0 {
            return String::from_utf16(&units)
                .map_err(|_| io::Error::other("invalid descriptor string"));
        }
        units.push(unit);
    }
    Err(io::Error::other("unterminated native descriptor string"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expected_descriptors_canonicalize_well_known_token_user_sids() {
        for directory in [false, true] {
            let descriptor = PrivateDescriptor::with_user(directory, "S-1-5-18").unwrap();
            assert!(descriptor.policy.starts_with("O:SYD:P("));
            assert!(!descriptor.policy.contains("S-1-5-18"));
        }
    }
}
