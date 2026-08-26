use super::{CapId, TableHeader};
use crate::{ABI_MINOR, CAP_KIND_FACTORY, RIGHT_MUTATE, RIGHT_RETAIN};
use core::ffi::c_void;

pub type ExchangeFn = Option<
    unsafe extern "C" fn(
        state: *mut c_void,
        opcode: u32,
        input: *const c_void,
        input_size: u32,
        output: *mut c_void,
        output_capacity: u32,
    ) -> u32,
>;

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct HostTable {
    pub header: TableHeader,
    pub issuer: u64,
    pub state: *mut c_void,
    pub exchange: ExchangeFn,
}

impl HostTable {
    #[allow(clippy::cast_possible_truncation)]
    pub const STRUCT_SIZE: u32 = core::mem::size_of::<Self>() as u32;
    #[allow(clippy::cast_possible_truncation)]
    pub const MIN_SIZE_V2_0: u32 =
        (core::mem::offset_of!(Self, exchange) + core::mem::size_of::<ExchangeFn>()) as u32;

    /// Checks a host table as consumed by an ABI v2.0 plugin.
    pub const fn is_compatible(&self) -> bool {
        self.is_compatible_for_plugin(ABI_MINOR)
    }

    /// Checks this host table for a plugin requiring `plugin_minor`.
    pub const fn is_compatible_for_plugin(&self, plugin_minor: u32) -> bool {
        self.header.has_common_shape()
            && self.header.abi_minor >= plugin_minor
            && self.header.struct_size >= Self::MIN_SIZE_V2_0
            && self.issuer != 0
            && !self.state.is_null()
            && self.exchange.is_some()
    }
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct PluginTable {
    pub header: TableHeader,
    pub issuer: u64,
    pub state: *mut c_void,
    pub exchange: ExchangeFn,
    pub factory: CapId,
}

impl PluginTable {
    #[allow(clippy::cast_possible_truncation)]
    pub const STRUCT_SIZE: u32 = core::mem::size_of::<Self>() as u32;
    #[allow(clippy::cast_possible_truncation)]
    pub const MIN_SIZE_V2_0: u32 =
        (core::mem::offset_of!(Self, factory) + core::mem::size_of::<CapId>()) as u32;
    pub const EMPTY: Self = Self {
        header: TableHeader {
            abi_major: 0,
            abi_minor: 0,
            struct_size: 0,
            flags: 0,
        },
        issuer: 0,
        state: core::ptr::null_mut(),
        exchange: None,
        factory: CapId::INVALID,
    };

    const fn minimum_size_for_minor(minor: u32) -> Option<u32> {
        match minor {
            ABI_MINOR => Some(Self::MIN_SIZE_V2_0),
            _ => None,
        }
    }

    /// Checks this plugin table from a host supporting `host_minor`.
    pub const fn is_compatible_for_host(&self, host_minor: u32) -> bool {
        self.header.has_common_shape()
            && self.header.abi_minor <= host_minor
            && matches!(
                Self::minimum_size_for_minor(self.header.abi_minor),
                Some(minimum) if self.header.struct_size >= minimum
            )
            && self.header.struct_size <= Self::STRUCT_SIZE
            && self.issuer != 0
            && !self.state.is_null()
            && self.exchange.is_some()
            && self.factory.issuer == self.issuer
            && self.factory.slot != 0
            && self.factory.epoch != 0
            && self.factory.kind == CAP_KIND_FACTORY
            && self.factory.rights == (RIGHT_RETAIN | RIGHT_MUTATE)
    }
}

pub type PluginEntryFn = unsafe extern "C" fn(
    host: *const HostTable,
    plugin_out: *mut PluginTable,
    output_capacity: u32,
) -> u32;
