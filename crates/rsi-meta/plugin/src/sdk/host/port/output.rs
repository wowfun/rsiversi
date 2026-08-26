use super::HostPort;
use crate::sdk::host::SdkError;
use crate::{
    CapId, FrameHeader, MAX_DIAGNOSTIC_BYTES, OutputPrefix, RIGHT_FINISH, RIGHT_RECEIVE,
    RIGHT_SEND, STATUS_PROTOCOL_ERROR,
};

pub(in crate::sdk::host) struct OutputGuard {
    port: HostPort,
    release: crate::ReleaseId,
    released: bool,
}

impl OutputGuard {
    pub(in crate::sdk::host) fn adopt(
        port: HostPort,
        prefix: OutputPrefix,
        expected: u32,
    ) -> Result<(Self, String), SdkError> {
        if !prefix.release.is_valid_or_empty() {
            return Err(SdkError::new(
                STATUS_PROTOCOL_ERROR,
                "host returned a partial output release token",
            ));
        }
        if !prefix.release.is_empty() && prefix.release.issuer != port.issuer() {
            return Err(SdkError::new(
                STATUS_PROTOCOL_ERROR,
                "foreign host output release token",
            ));
        }
        let guard = Self {
            port,
            release: prefix.release,
            released: false,
        };
        prefix
            .validate(expected, expected, MAX_DIAGNOSTIC_BYTES)
            .map_err(|error| SdkError::new(STATUS_PROTOCOL_ERROR, error.to_string()))?;
        let diagnostic = copy_bytes(prefix.diagnostic, MAX_DIAGNOSTIC_BYTES)?;
        Ok((guard, diagnostic))
    }

    pub(in crate::sdk::host) fn release(mut self) -> Result<(), SdkError> {
        self.released = true;
        self.port.release_output(self.release)
    }
}

impl Drop for OutputGuard {
    fn drop(&mut self) {
        if !self.released {
            let _ = self.port.release_output(self.release);
            self.released = true;
        }
    }
}

pub(in crate::sdk::host) fn validate_cap(
    capability: CapId,
    issuer: u64,
    kind: u32,
    rights: u32,
) -> Result<(), SdkError> {
    if capability.is_structurally_valid()
        && capability.issuer == issuer
        && capability.kind == kind
        && capability.rights == rights
    {
        Ok(())
    } else {
        Err(SdkError::new(
            STATUS_PROTOCOL_ERROR,
            "host returned wrong capability metadata",
        ))
    }
}

fn copy_bytes(raw: crate::RawBytes, maximum: usize) -> Result<String, SdkError> {
    let length = raw
        .checked_len(maximum)
        .map_err(|error| SdkError::new(STATUS_PROTOCOL_ERROR, error.to_string()))?;
    if length == 0 {
        return Ok(String::new());
    }
    // SAFETY: The host output token keeps the structurally validated range live.
    let bytes = unsafe { std::slice::from_raw_parts(raw.ptr, length) };
    String::from_utf8(bytes.to_vec())
        .map_err(|_| SdkError::new(STATUS_PROTOCOL_ERROR, "host diagnostic is not UTF-8"))
}

pub(in crate::sdk::host) fn frame<T>() -> FrameHeader {
    FrameHeader::new(size_u32::<T>())
}

pub(in crate::sdk::host) fn size_u32<T>() -> u32 {
    u32::try_from(size_of::<T>()).expect("native ABI frame size exceeds u32")
}

pub(in crate::sdk::host) const CALL_CHANNEL_RIGHTS: u32 = RIGHT_RECEIVE | RIGHT_SEND | RIGHT_FINISH;
