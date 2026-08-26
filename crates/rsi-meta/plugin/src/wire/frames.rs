use super::{CapId, Injection, RawBytes, RawMessage, RawRequirement, ReleaseId, WireError};

#[repr(C)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FrameHeader {
    pub struct_size: u32,
    pub reserved: u32,
}

impl FrameHeader {
    pub const fn new(struct_size: u32) -> Self {
        Self {
            struct_size,
            reserved: 0,
        }
    }

    pub const fn is_compatible(&self, minimum_size: u32, input_size: u32) -> bool {
        self.reserved == 0 && self.struct_size >= minimum_size && self.struct_size == input_size
    }
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct OutputPrefix {
    pub struct_size: u32,
    pub reserved: u32,
    pub release: ReleaseId,
    pub diagnostic: RawBytes,
}

impl OutputPrefix {
    pub const fn empty(struct_size: u32) -> Self {
        Self {
            struct_size,
            reserved: 0,
            release: ReleaseId::EMPTY,
            diagnostic: RawBytes::EMPTY,
        }
    }

    pub fn validate(
        &self,
        minimum_size: u32,
        output_capacity: u32,
        maximum_diagnostic: usize,
    ) -> Result<(), WireError> {
        if self.reserved != 0
            || self.struct_size < minimum_size
            || self.struct_size > output_capacity
        {
            return Err(WireError::InvalidHeader);
        }
        if !self.release.is_valid_or_empty() {
            return Err(WireError::InvalidRelease);
        }
        self.diagnostic.checked_len(maximum_diagnostic)?;
        Ok(())
    }
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CapInput {
    pub header: FrameHeader,
    pub capability: CapId,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct OpenInput {
    pub header: FrameHeader,
    pub scope: CapId,
    pub service: CapId,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct EmptyInput {
    pub header: FrameHeader,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ReleaseOutputInput {
    pub header: FrameHeader,
    pub release: ReleaseId,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BytesInput {
    pub header: FrameHeader,
    pub receiver: CapId,
    pub bytes: RawBytes,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MessageInput {
    pub header: FrameHeader,
    pub channel: CapId,
    pub message: RawMessage,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ActivateInput {
    pub header: FrameHeader,
    pub callback_id: u64,
    pub instance: CapId,
    pub activation: CapId,
    pub injections: *const Injection,
    pub injection_count: u64,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ServeInput {
    pub header: FrameHeader,
    pub callback_id: u64,
    pub instance: CapId,
    pub provider: CapId,
    pub port: RawBytes,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct EffectDeferInput {
    pub header: FrameHeader,
    pub transaction: CapId,
    pub cleanup: CapId,
    pub label: RawBytes,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ProvideInput {
    pub header: FrameHeader,
    pub transaction: CapId,
    pub port: RawBytes,
    pub key: RawBytes,
    pub contract: RawBytes,
    pub version: u64,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BasicOutput {
    pub prefix: OutputPrefix,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BytesOutput {
    pub prefix: OutputPrefix,
    pub bytes: RawBytes,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CapOutput {
    pub prefix: OutputPrefix,
    pub capability: CapId,
}

impl CapOutput {
    pub const fn validate_capability_shape(&self) -> Result<(), WireError> {
        if self.capability.has_owned_output_shape() {
            Ok(())
        } else {
            Err(WireError::InvalidCapability)
        }
    }
}

/// Callback-frame-owned capability output with no retain authority.
#[repr(C)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BorrowedCapOutput {
    pub prefix: OutputPrefix,
    pub capability: CapId,
}

impl BorrowedCapOutput {
    pub const fn validate_capability_shape(&self) -> Result<(), WireError> {
        if self.capability.has_borrowed_output_shape() {
            Ok(())
        } else {
            Err(WireError::InvalidCapability)
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MessageOutput {
    pub prefix: OutputPrefix,
    pub present: u32,
    pub reserved: u32,
    pub message: RawMessage,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BoolOutput {
    pub prefix: OutputPrefix,
    pub value: u32,
    pub reserved: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PrepareOutput {
    pub prefix: OutputPrefix,
    pub prepared: CapId,
    pub normalized_config: RawBytes,
    pub requirements: *const RawRequirement,
    pub requirement_count: u64,
    pub retained_bytes: u64,
}
