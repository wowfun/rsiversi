//! Minimal C ABI and safe Rust authoring surface for trusted native plugins.
//!
//! The ABI carries only fixed-width integers, opaque handles, pointer/length
//! pairs, and function pointers. Context, Fiber, routing, and cleanup stay in
//! the host. Native code implements a factory and byte-oriented service calls.

#![deny(unsafe_op_in_unsafe_fn, clippy::undocumented_unsafe_blocks)]
#![allow(unsafe_code)] // This crate is the deliberately audited C ABI boundary.
#![allow(clippy::missing_errors_doc, clippy::missing_panics_doc)]

use core::ffi::c_void;

mod sdk;

pub use sdk::{Host, NativeInstance, NativePlugin, borrow_abi_input, copy_buffer, plugin_api};

pub const ABI_MAJOR: u32 = 1;
pub const ABI_MINOR: u32 = 0;
pub const PLUGIN_ENTRY_SYMBOL: &[u8] = b"rsi_meta_plugin_entry_v1\0";

pub const STATUS_OK: u32 = 0;
pub const STATUS_INVALID_ARGUMENT: u32 = 1;
pub const STATUS_FAILED: u32 = 2;
pub const STATUS_PANICKED: u32 = 3;

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct Buffer {
    pub ptr: *mut u8,
    pub len: usize,
    pub capacity: usize,
}

impl Buffer {
    pub const EMPTY: Self = Self {
        ptr: core::ptr::null_mut(),
        len: 0,
        capacity: 0,
    };

    pub fn from_vec(mut bytes: Vec<u8>) -> Self {
        let buffer = Self {
            ptr: bytes.as_mut_ptr(),
            len: bytes.len(),
            capacity: bytes.capacity(),
        };
        core::mem::forget(bytes);
        buffer
    }

    /// Reclaims a buffer created by [`Buffer::from_vec`] in the same module.
    ///
    /// # Safety
    ///
    /// Call this at most once through the allocator's release callback. The
    /// buffer fields must be unchanged from the value returned by its owner.
    pub unsafe fn reclaim(self) {
        if self.capacity == 0 {
            return;
        }
        // SAFETY: The caller upholds allocation provenance and unique ownership.
        drop(unsafe { Vec::from_raw_parts(self.ptr, self.len, self.capacity) });
    }
}

pub type HostCallServiceFn = unsafe extern "C" fn(
    host_handle: *mut c_void,
    service_ptr: *const u8,
    service_len: usize,
    request_ptr: *const u8,
    request_len: usize,
    response_out: *mut Buffer,
) -> u32;
pub type ReleaseBufferFn = unsafe extern "C" fn(buffer: Buffer);

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct HostApi {
    pub abi_major: u32,
    pub abi_minor: u32,
    pub struct_size: u32,
    pub reserved: u32,
    pub host_handle: *mut c_void,
    pub call_service: Option<HostCallServiceFn>,
    pub release_buffer: Option<ReleaseBufferFn>,
}

impl HostApi {
    #[allow(clippy::cast_possible_truncation)]
    pub const STRUCT_SIZE: u32 = core::mem::size_of::<Self>() as u32;
    #[allow(clippy::cast_possible_truncation)]
    pub const MIN_SIZE_V1_0: u32 = (core::mem::offset_of!(Self, release_buffer)
        + core::mem::size_of::<Option<ReleaseBufferFn>>())
        as u32;

    const fn minimum_size_for_minor(minor: u32) -> Option<u32> {
        match minor {
            0 => Some(Self::MIN_SIZE_V1_0),
            _ => None,
        }
    }

    pub const fn is_compatible(&self) -> bool {
        self.abi_major == ABI_MAJOR
            && self.abi_minor.checked_sub(ABI_MINOR).is_some()
            && matches!(
                Self::minimum_size_for_minor(ABI_MINOR),
                Some(minimum) if self.struct_size >= minimum
            )
            && self.reserved == 0
            && !self.host_handle.is_null()
            && self.call_service.is_some()
            && self.release_buffer.is_some()
    }
}

pub type DescriptorFn = unsafe extern "C" fn(*mut c_void, *mut Buffer) -> u32;
pub type ValidateConfigFn = unsafe extern "C" fn(*mut c_void, *const u8, usize, *mut Buffer) -> u32;
pub type CreateFn =
    unsafe extern "C" fn(*mut c_void, *const u8, usize, *mut *mut c_void, *mut Buffer) -> u32;
pub type CallFn = unsafe extern "C" fn(
    *mut c_void,
    *const HostApi,
    *const u8,
    usize,
    *const u8,
    usize,
    *mut Buffer,
) -> u32;
pub type DestroyFn = unsafe extern "C" fn(*mut c_void);

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct PluginApi {
    pub abi_major: u32,
    pub abi_minor: u32,
    pub struct_size: u32,
    pub reserved: u32,
    pub factory_handle: *mut c_void,
    pub descriptor: Option<DescriptorFn>,
    pub validate_config: Option<ValidateConfigFn>,
    pub create: Option<CreateFn>,
    pub call: Option<CallFn>,
    pub destroy_instance: Option<DestroyFn>,
    pub destroy_factory: Option<DestroyFn>,
    pub release_buffer: Option<ReleaseBufferFn>,
}

impl PluginApi {
    #[allow(clippy::cast_possible_truncation)]
    pub const STRUCT_SIZE: u32 = core::mem::size_of::<Self>() as u32;
    #[allow(clippy::cast_possible_truncation)]
    pub const MIN_SIZE_V1_0: u32 = (core::mem::offset_of!(Self, release_buffer)
        + core::mem::size_of::<Option<ReleaseBufferFn>>())
        as u32;
    pub const EMPTY: Self = Self {
        abi_major: 0,
        abi_minor: 0,
        struct_size: 0,
        reserved: 0,
        factory_handle: core::ptr::null_mut(),
        descriptor: None,
        validate_config: None,
        create: None,
        call: None,
        destroy_instance: None,
        destroy_factory: None,
        release_buffer: None,
    };

    const fn minimum_size_for_minor(minor: u32) -> Option<u32> {
        match minor {
            0 => Some(Self::MIN_SIZE_V1_0),
            _ => None,
        }
    }

    pub const fn is_compatible(&self) -> bool {
        self.abi_major == ABI_MAJOR
            && ABI_MINOR.checked_sub(self.abi_minor).is_some()
            && matches!(
                Self::minimum_size_for_minor(self.abi_minor),
                Some(minimum) if self.struct_size >= minimum
            )
            && self.reserved == 0
            && !self.factory_handle.is_null()
            && self.descriptor.is_some()
            && self.validate_config.is_some()
            && self.create.is_some()
            && self.call.is_some()
            && self.destroy_instance.is_some()
            && self.destroy_factory.is_some()
            && self.release_buffer.is_some()
    }
}

pub type PluginEntryFn = unsafe extern "C" fn(*mut PluginApi, usize) -> u32;

#[macro_export]
macro_rules! export_plugin {
    ($plugin:ty) => {
        #[unsafe(no_mangle)]
        pub unsafe extern "C" fn rsi_meta_plugin_entry_v1(
            output: *mut $crate::PluginApi,
            capacity: usize,
        ) -> u32 {
            if output.is_null() || capacity < core::mem::size_of::<$crate::PluginApi>() {
                return $crate::STATUS_INVALID_ARGUMENT;
            }
            let api = $crate::plugin_api::<$plugin>();
            // SAFETY: The caller supplied checked writable output storage.
            unsafe { output.write(api) };
            if api.is_compatible() {
                $crate::STATUS_OK
            } else {
                $crate::STATUS_PANICKED
            }
        }
    };
}
