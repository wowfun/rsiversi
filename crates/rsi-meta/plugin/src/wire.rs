use crate::ABI_MAJOR;
use core::fmt;

mod frames;
mod tables;

pub use frames::*;
pub use tables::*;

/// Structural failure detected before dereferencing a native wire value.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WireError {
    NullForNonempty,
    LengthOverflow,
    LimitExceeded,
    Misaligned,
    InvalidHeader,
    InvalidRelease,
    InvalidCapability,
}

impl fmt::Display for WireError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::NullForNonempty => "nonempty native range has a null pointer",
            Self::LengthOverflow => "native length cannot be represented safely",
            Self::LimitExceeded => "native value exceeds its configured limit",
            Self::Misaligned => "native array pointer is misaligned",
            Self::InvalidHeader => "native frame header is invalid",
            Self::InvalidRelease => "native release token is malformed",
            Self::InvalidCapability => "native capability metadata is malformed",
        })
    }
}

impl std::error::Error for WireError {}

#[repr(C)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TableHeader {
    pub abi_major: u32,
    pub abi_minor: u32,
    pub struct_size: u32,
    pub flags: u32,
}

impl TableHeader {
    pub const fn new(abi_minor: u32, struct_size: u32) -> Self {
        Self {
            abi_major: ABI_MAJOR,
            abi_minor,
            struct_size,
            flags: 0,
        }
    }

    const fn has_common_shape(&self) -> bool {
        self.abi_major == ABI_MAJOR && self.flags == 0
    }
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct CapId {
    pub issuer: u64,
    pub slot: u64,
    pub epoch: u64,
    pub kind: u32,
    pub rights: u32,
}

impl CapId {
    pub const INVALID: Self = Self {
        issuer: 0,
        slot: 0,
        epoch: 0,
        kind: 0,
        rights: 0,
    };

    pub const fn is_structurally_valid(&self) -> bool {
        self.issuer != 0
            && self.slot != 0
            && self.epoch != 0
            && self.kind != 0
            && self.rights != 0
            && self.rights & !crate::KNOWN_RIGHTS == 0
    }

    pub(crate) const fn has_owned_output_shape(&self) -> bool {
        self.is_structurally_valid() && self.rights & crate::RIGHT_RETAIN != 0
    }

    pub(crate) const fn has_borrowed_output_shape(&self) -> bool {
        self.is_structurally_valid() && self.rights & crate::RIGHT_RETAIN == 0
    }
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct ReleaseId {
    pub issuer: u64,
    pub slot: u64,
    pub epoch: u64,
}

impl ReleaseId {
    pub const EMPTY: Self = Self {
        issuer: 0,
        slot: 0,
        epoch: 0,
    };

    pub const fn is_empty(&self) -> bool {
        self.issuer == 0 && self.slot == 0 && self.epoch == 0
    }

    pub const fn is_valid_or_empty(&self) -> bool {
        self.is_empty() || (self.issuer != 0 && self.slot != 0 && self.epoch != 0)
    }
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RawBytes {
    pub ptr: *const u8,
    pub len: u64,
}

impl RawBytes {
    pub const EMPTY: Self = Self {
        ptr: core::ptr::null(),
        len: 0,
    };

    pub fn checked_len(&self, maximum: usize) -> Result<usize, WireError> {
        let length = usize::try_from(self.len).map_err(|_| WireError::LengthOverflow)?;
        if length > isize::MAX as usize {
            return Err(WireError::LengthOverflow);
        }
        if length > maximum {
            return Err(WireError::LimitExceeded);
        }
        if length != 0 && self.ptr.is_null() {
            return Err(WireError::NullForNonempty);
        }
        Ok(length)
    }
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RawMessage {
    pub bytes: RawBytes,
    pub capabilities: *const CapId,
    pub capability_count: u64,
}

impl RawMessage {
    pub fn validate_shape(
        &self,
        maximum_bytes: usize,
        maximum_capabilities: usize,
    ) -> Result<(usize, usize), WireError> {
        let bytes = self.bytes.checked_len(maximum_bytes)?;
        let capabilities =
            usize::try_from(self.capability_count).map_err(|_| WireError::LengthOverflow)?;
        if capabilities > maximum_capabilities {
            return Err(WireError::LimitExceeded);
        }
        let capability_bytes = capabilities
            .checked_mul(core::mem::size_of::<CapId>())
            .ok_or(WireError::LengthOverflow)?;
        if capability_bytes > isize::MAX as usize {
            return Err(WireError::LengthOverflow);
        }
        if capabilities != 0 && self.capabilities.is_null() {
            return Err(WireError::NullForNonempty);
        }
        if capabilities != 0
            && !self
                .capabilities
                .addr()
                .is_multiple_of(core::mem::align_of::<CapId>())
        {
            return Err(WireError::Misaligned);
        }
        Ok((bytes, capabilities))
    }
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RawRequirement {
    pub key: RawBytes,
    pub contract: RawBytes,
    pub version: u64,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Injection {
    pub requirement_index: u64,
    pub service: CapId,
}
