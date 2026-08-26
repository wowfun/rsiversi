use super::{CALL_CHANNEL_RIGHTS, HostPort, OutputGuard, frame, size_u32, validate_cap};
use crate::sdk::host::{Capability, Message, SdkError};
use crate::{
    BoolOutput, CAP_KIND_CALL_CHANNEL, CAP_KIND_SERVICE, CapId, CapInput, HOST_CAP_OPEN,
    HOST_CHANNEL_CANCELLED, HOST_CHANNEL_FINISH_REQUESTS, HOST_CHANNEL_RECV, HOST_CHANNEL_SEND,
    HOST_CHANNEL_TERMINAL, MessageInput, MessageOutput, OpenInput, RIGHT_OPEN, RIGHT_RETAIN,
    RawBytes, RawMessage, STATUS_OK, STATUS_PROTOCOL_ERROR,
};

const MAX_MESSAGE_BYTES: usize = 1_048_576;
const MAX_MESSAGE_CAPABILITIES: usize = 1_024;

impl HostPort {
    pub(crate) fn open(&self, scope: CapId, service: CapId) -> Result<CapId, SdkError> {
        self.borrowed_cap(
            HOST_CAP_OPEN,
            &OpenInput {
                header: frame::<OpenInput>(),
                scope,
                service,
            },
            CAP_KIND_CALL_CHANNEL,
            CALL_CHANNEL_RIGHTS,
        )
    }

    pub(crate) fn receive(&self, channel: CapId) -> Result<Option<Message>, SdkError> {
        let input = CapInput {
            header: frame::<CapInput>(),
            capability: channel,
        };
        // SAFETY: `MessageOutput` contains only integers and raw pointers, for
        // which the all-zero bit pattern is valid. Its zero header ensures an
        // unwritten output cannot pass validation.
        let mut output: MessageOutput = unsafe { core::mem::zeroed() };
        let status = self.call(HOST_CHANNEL_RECV, &input, &mut output);
        let (guard, diagnostic) =
            OutputGuard::adopt(*self, output.prefix, size_u32::<MessageOutput>())?;
        if status != STATUS_OK {
            let _ = guard.release();
            return Err(SdkError::new(status, diagnostic));
        }
        if output.reserved != 0 || output.present > 1 {
            let _ = guard.release();
            return Err(protocol("host returned malformed message presence"));
        }
        if output.present == 0 {
            guard.release()?;
            return Ok(None);
        }
        let (byte_count, cap_count) = output
            .message
            .validate_shape(MAX_MESSAGE_BYTES, MAX_MESSAGE_CAPABILITIES)
            .map_err(|error| protocol(error.to_string()))?;
        // SAFETY: The adopted output token keeps both validated ranges live.
        let bytes = unsafe { copy_raw(output.message.bytes, byte_count) };
        let raw_caps = if cap_count == 0 {
            &[][..]
        } else {
            // SAFETY: Shape validation established a non-null, aligned
            // capability array. The zero-count ABI representation is handled
            // above because its canonical pointer is null.
            unsafe { std::slice::from_raw_parts(output.message.capabilities, cap_count) }
        };
        for capability in raw_caps {
            validate_cap(
                *capability,
                self.issuer(),
                CAP_KIND_SERVICE,
                RIGHT_RETAIN | RIGHT_OPEN,
            )?;
        }
        let mut retained = Vec::with_capacity(raw_caps.len());
        for capability in raw_caps {
            if let Err(error) = self.retain(*capability) {
                for accepted in retained.drain(..) {
                    let _ = self.release(accepted);
                }
                let _ = guard.release();
                return Err(error);
            }
            retained.push(*capability);
        }
        if let Err(error) = guard.release() {
            for capability in retained {
                let _ = self.release(capability);
            }
            return Err(error);
        }
        Ok(Some(Message {
            bytes,
            capabilities: retained
                .into_iter()
                .map(|capability| Capability::new(*self, capability))
                .collect(),
        }))
    }

    pub(crate) fn send(&self, channel: CapId, message: &Message) -> Result<(), SdkError> {
        if message.bytes.len() > MAX_MESSAGE_BYTES
            || message.capabilities.len() > MAX_MESSAGE_CAPABILITIES
        {
            return Err(SdkError::new(
                crate::STATUS_LIMIT_EXCEEDED,
                "native message exceeds SDK bounds",
            ));
        }
        let mut capabilities = Vec::with_capacity(message.capabilities.len());
        for capability in &message.capabilities {
            if capability.port.issuer() != self.issuer() {
                return Err(SdkError::new(
                    crate::STATUS_WRONG_CAPABILITY,
                    "message capability belongs to another host",
                ));
            }
            validate_cap(
                capability.id,
                self.issuer(),
                CAP_KIND_SERVICE,
                RIGHT_RETAIN | RIGHT_OPEN,
            )?;
            capabilities.push(capability.id);
        }
        let input = MessageInput {
            header: frame::<MessageInput>(),
            channel,
            message: RawMessage {
                bytes: raw_bytes(&message.bytes),
                capabilities: if capabilities.is_empty() {
                    core::ptr::null()
                } else {
                    capabilities.as_ptr()
                },
                capability_count: u64::try_from(capabilities.len()).unwrap_or(u64::MAX),
            },
        };
        self.call_basic(HOST_CHANNEL_SEND, &input)
    }

    pub(crate) fn finish(&self, channel: CapId) -> Result<(), SdkError> {
        self.channel_basic(HOST_CHANNEL_FINISH_REQUESTS, channel)
    }

    pub(crate) fn terminal(&self, channel: CapId) -> Result<(), SdkError> {
        self.channel_basic(HOST_CHANNEL_TERMINAL, channel)
    }

    fn channel_basic(&self, opcode: u32, channel: CapId) -> Result<(), SdkError> {
        self.call_basic(
            opcode,
            &CapInput {
                header: frame::<CapInput>(),
                capability: channel,
            },
        )
    }

    pub(crate) fn cancelled(&self, channel: CapId) -> Result<bool, SdkError> {
        let input = CapInput {
            header: frame::<CapInput>(),
            capability: channel,
        };
        // SAFETY: `BoolOutput` contains only integers and raw pointers, for
        // which the all-zero bit pattern is valid. Its zero header ensures an
        // unwritten output cannot pass validation.
        let mut output: BoolOutput = unsafe { core::mem::zeroed() };
        let status = self.call(HOST_CHANNEL_CANCELLED, &input, &mut output);
        let (guard, diagnostic) =
            OutputGuard::adopt(*self, output.prefix, size_u32::<BoolOutput>())?;
        let result = if status != STATUS_OK {
            Err(SdkError::new(status, diagnostic))
        } else if output.reserved != 0 || output.value > 1 {
            Err(protocol("host returned malformed boolean"))
        } else {
            Ok(output.value == 1)
        };
        let release = guard.release();
        result.and(release.map(|()| output.value == 1))
    }
}

/// # Safety
///
/// For nonzero `length`, `raw.ptr` must remain readable for exactly `length`
/// bytes for this synchronous copy.
unsafe fn copy_raw(raw: RawBytes, length: usize) -> Vec<u8> {
    if length == 0 {
        Vec::new()
    } else {
        // SAFETY: Caller validated and adopted the complete raw range.
        unsafe { std::slice::from_raw_parts(raw.ptr, length) }.to_vec()
    }
}

fn raw_bytes(bytes: &[u8]) -> RawBytes {
    RawBytes {
        ptr: if bytes.is_empty() {
            core::ptr::null()
        } else {
            bytes.as_ptr()
        },
        len: u64::try_from(bytes.len()).unwrap_or(u64::MAX),
    }
}

fn protocol(message: impl Into<String>) -> SdkError {
    SdkError::new(STATUS_PROTOCOL_ERROR, message)
}
