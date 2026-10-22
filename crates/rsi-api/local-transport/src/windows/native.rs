use std::{io, os::windows::io::AsRawHandle as _};
use tokio::net::windows::named_pipe::{NamedPipeClient, NamedPipeServer, ServerOptions};
use windows_sys::Win32::{
    Foundation::LocalFree,
    Security::{
        Authorization::ConvertStringSecurityDescriptorToSecurityDescriptorW, SECURITY_ATTRIBUTES,
    },
    System::Pipes::{GetNamedPipeClientProcessId, GetNamedPipeServerProcessId},
};

struct Descriptor(*mut std::ffi::c_void);
impl Drop for Descriptor {
    fn drop(&mut self) {
        // SAFETY: The successful SDDL conversion allocated this descriptor with LocalAlloc.
        unsafe { LocalFree(self.0) };
    }
}

pub(super) fn create(name: &str, sid: &str, first: bool) -> io::Result<NamedPipeServer> {
    let sddl: Vec<u16> = format!("O:{sid}D:P(A;;GA;;;{sid})")
        .encode_utf16()
        .chain(Some(0))
        .collect();
    let mut descriptor = std::ptr::null_mut();
    // SAFETY: SDDL is null-terminated, revision 1 is supported, and the output pointer is live.
    if unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            sddl.as_ptr(),
            1,
            &raw mut descriptor,
            std::ptr::null_mut(),
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    let descriptor = Descriptor(descriptor);
    let attributes = SECURITY_ATTRIBUTES {
        nLength: u32::try_from(std::mem::size_of::<SECURITY_ATTRIBUTES>())
            .map_err(io::Error::other)?,
        lpSecurityDescriptor: descriptor.0,
        bInheritHandle: 0,
    };
    // SAFETY: SECURITY_ATTRIBUTES and its valid converted descriptor remain live
    // through CreateNamedPipeW inside Tokio. The resulting handle is not inherited.
    unsafe {
        ServerOptions::new()
            .first_pipe_instance(first)
            .reject_remote_clients(true)
            .in_buffer_size(32 * 1024)
            .out_buffer_size(32 * 1024)
            .create_with_security_attributes_raw(name, (&raw const attributes).cast_mut().cast())
    }
}

pub(super) fn client_pid(pipe: &NamedPipeServer) -> io::Result<u32> {
    let mut pid = 0;
    // SAFETY: The connected server owns a valid pipe handle and the PID output remains live.
    if unsafe { GetNamedPipeClientProcessId(pipe.as_raw_handle(), &raw mut pid) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(pid)
}
pub(super) fn server_pid(pipe: &NamedPipeClient) -> io::Result<u32> {
    let mut pid = 0;
    // SAFETY: The connected client owns a valid pipe handle and the PID output remains live.
    if unsafe { GetNamedPipeServerProcessId(pipe.as_raw_handle(), &raw mut pid) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(pid)
}
