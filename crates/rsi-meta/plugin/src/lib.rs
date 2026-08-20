//! Fixed-layout C ABI and safe Rust SDK for trusted `rsi-meta` plugins.
//!
//! The raw ABI intentionally contains only fixed-width integers, opaque
//! handles, pointers paired with lengths, and C function pointers. Rust-owned
//! containers, trait objects, references, and unwinding never cross it.
//!
//! This is an experimental v0 SDK and ABI. Compatibility is gated by the
//! declared ABI table at load time, not promised across `rsi-meta` releases.

#![deny(unsafe_op_in_unsafe_fn)]
#![deny(clippy::undocumented_unsafe_blocks)]
#![allow(unsafe_code)] // This crate is the audited C-ABI boundary.
#![warn(missing_debug_implementations)]

use core::ffi::c_void;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

/// ABI major version implemented by this crate.
pub const ABI_MAJOR: u32 = 0;
/// ABI minor version implemented by this crate.
pub const ABI_MINOR: u32 = 0;
/// The only symbol a loader resolves from a plugin dylib.
pub const PLUGIN_ENTRY_SYMBOL: &[u8] = b"rsi_meta_plugin_entry_v0\0";

/// Canonical description used to detect accidental ABI layout changes.
pub const ABI_LAYOUT_DESCRIPTOR: &str = "rsi-meta-plugin-abi;major=0;minor=0;word=u32;size=size_t;host={abi_major,abi_minor,struct_size,reserved,host_handle,host_post_frame};plugin={abi_major,abi_minor,struct_size,reserved,plugin_handle,on_frame,shutdown,destroy};entry=rsi_meta_plugin_entry_v0:fn(host*,plugin*,plugin_capacity)->u32;lanes={control:0,data:1};post={accepted:0,would_block:1,closed:2};call={ok:0,invalid_argument:1,closed:2,failed:3,panicked:4};init={ok:0,invalid_host_api:1,rejected:2,panicked:3}";
/// SHA-256 of [`ABI_LAYOUT_DESCRIPTOR`].
pub const ABI_LAYOUT_SHA256: &str =
    "87fff0f82faef21695613380f4aab233bfb9a273a3c255ab5c439a9dff1589f5";

/// The maintained C header for this ABI.
pub const C_HEADER: &str = include_str!("../include/rsi_meta_plugin.h");

const fn supports_minor(available: u32, minimum: u32) -> bool {
    available >= minimum
}

/// Control-plane frame lane.
pub const LANE_CONTROL: u32 = 0;
/// Data-plane frame lane.
pub const LANE_DATA: u32 = 1;
/// Maximum outstanding DATA bytes in either direction of one service stream.
///
/// This is part of the shared host/plugin wire contract rather than a host
/// implementation detail, so both sides must enforce the same value.
pub const STREAM_BYTE_BUDGET: u64 = 16 * 1024 * 1024;

/// The host accepted and copied the frame.
pub const POST_FRAME_ACCEPTED: u32 = 0;
/// The attempt exceeds current queue or frame capacity.
///
/// Queue saturation may be retried after progress. An oversized frame must be
/// re-encoded within the host's configured maximum before retrying.
pub const POST_FRAME_WOULD_BLOCK: u32 = 1;
/// The receiving side of the lane is permanently closed.
pub const POST_FRAME_CLOSED: u32 = 2;

/// A plugin callback completed successfully.
pub const CALL_OK: u32 = 0;
/// A callback argument was invalid.
pub const CALL_INVALID_ARGUMENT: u32 = 1;
/// The plugin has already shut down.
pub const CALL_CLOSED: u32 = 2;
/// Plugin code returned an application error.
pub const CALL_FAILED: u32 = 3;
/// A Rust panic was caught at the SDK's FFI trampoline.
pub const CALL_PANICKED: u32 = 4;

/// Plugin initialization completed successfully.
pub const INIT_OK: u32 = 0;
/// The host table is null, too small, or ABI-incompatible.
pub const INIT_INVALID_HOST_API: u32 = 1;
/// Plugin construction returned an application error.
pub const INIT_REJECTED: u32 = 2;
/// A Rust panic was caught while constructing the plugin.
pub const INIT_PANICKED: u32 = 3;

/// Host callback used by a plugin to post a frame.
///
/// The host must copy `data_len` bytes before returning. It must not retain
/// `data_ptr`.
pub type HostPostFrameFn = unsafe extern "C" fn(
    host_handle: *mut c_void,
    lane: u32,
    data_ptr: *const u8,
    data_len: usize,
) -> u32;

/// Host-owned function table passed to the plugin entry point.
///
/// `struct_size` permits appending fields in a future compatible minor ABI.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct HostApi {
    pub abi_major: u32,
    pub abi_minor: u32,
    pub struct_size: u32,
    pub reserved: u32,
    pub host_handle: *mut c_void,
    pub host_post_frame: Option<HostPostFrameFn>,
}

impl HostApi {
    /// Size of the currently defined host table prefix.
    #[allow(clippy::cast_possible_truncation)] // ABI tables are statically far below u32::MAX.
    pub const STRUCT_SIZE: u32 = core::mem::size_of::<Self>() as u32;

    /// Constructs a host table.
    ///
    /// # Safety
    ///
    /// `host_handle` and `host_post_frame` must remain valid until the plugin's
    /// `destroy` callback returns. The opaque context and callback must be safe
    /// to send to and invoke concurrently from arbitrary plugin threads, must
    /// copy the frame synchronously, and must never unwind across the C ABI.
    pub const unsafe fn new(host_handle: *mut c_void, host_post_frame: HostPostFrameFn) -> Self {
        Self {
            abi_major: ABI_MAJOR,
            abi_minor: ABI_MINOR,
            struct_size: Self::STRUCT_SIZE,
            reserved: 0,
            host_handle,
            host_post_frame: Some(host_post_frame),
        }
    }

    /// Returns whether the known table prefix is present and compatible.
    pub const fn is_compatible(&self) -> bool {
        self.abi_major == ABI_MAJOR
            && supports_minor(self.abi_minor, ABI_MINOR)
            && self.struct_size >= Self::STRUCT_SIZE
            && self.reserved == 0
            && !self.host_handle.is_null()
            && self.host_post_frame.is_some()
    }
}

/// Delivers a host frame to a plugin instance.
///
/// The host serializes callbacks for one `plugin_handle`, but may move that
/// serialized invocation between host threads. Implementations must therefore
/// keep the handle movable across threads and must not rely on thread affinity.
pub type PluginOnFrameFn = unsafe extern "C" fn(
    plugin_handle: *mut c_void,
    lane: u32,
    data_ptr: *const u8,
    data_len: usize,
) -> u32;

/// Requests graceful shutdown without freeing the opaque handle.
pub type PluginShutdownFn = unsafe extern "C" fn(plugin_handle: *mut c_void) -> u32;

/// Frees the plugin instance. The host must call this at most once.
pub type PluginDestroyFn = unsafe extern "C" fn(plugin_handle: *mut c_void) -> u32;

/// Plugin-owned function table written by the plugin entry point.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct PluginApi {
    pub abi_major: u32,
    pub abi_minor: u32,
    pub struct_size: u32,
    pub reserved: u32,
    pub plugin_handle: *mut c_void,
    pub on_frame: Option<PluginOnFrameFn>,
    pub shutdown: Option<PluginShutdownFn>,
    pub destroy: Option<PluginDestroyFn>,
}

impl PluginApi {
    /// Size of the currently defined plugin table prefix.
    #[allow(clippy::cast_possible_truncation)] // ABI tables are statically far below u32::MAX.
    pub const STRUCT_SIZE: u32 = core::mem::size_of::<Self>() as u32;

    /// Zeroed output value used before calling a plugin entry point.
    pub const EMPTY: Self = Self {
        abi_major: 0,
        abi_minor: 0,
        struct_size: 0,
        reserved: 0,
        plugin_handle: core::ptr::null_mut(),
        on_frame: None,
        shutdown: None,
        destroy: None,
    };

    /// Returns whether all mandatory fields of the current ABI are present.
    pub const fn is_compatible(&self) -> bool {
        self.abi_major == ABI_MAJOR
            && supports_minor(ABI_MINOR, self.abi_minor)
            && self.struct_size >= Self::STRUCT_SIZE
            && self.reserved == 0
            && !self.plugin_handle.is_null()
            && self.on_frame.is_some()
            && self.destroy.is_some()
    }
}

/// Signature of [`PLUGIN_ENTRY_SYMBOL`].
pub type PluginEntryFn = unsafe extern "C" fn(
    host_api: *const HostApi,
    plugin_api_out: *mut PluginApi,
    plugin_api_capacity: usize,
) -> u32;

/// A validated frame lane for safe Rust plugin code.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Lane {
    Control,
    Data,
}

impl Lane {
    pub const fn as_raw(self) -> u32 {
        match self {
            Self::Control => LANE_CONTROL,
            Self::Data => LANE_DATA,
        }
    }

    pub const fn from_raw(raw: u32) -> Option<Self> {
        match raw {
            LANE_CONTROL => Some(Self::Control),
            LANE_DATA => Some(Self::Data),
            _ => None,
        }
    }
}

/// Backpressure result returned by `host_post_frame`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PostFrameOutcome {
    Accepted,
    WouldBlock,
    Closed,
    Unknown(u32),
}

impl PostFrameOutcome {
    pub const fn from_raw(raw: u32) -> Self {
        match raw {
            POST_FRAME_ACCEPTED => Self::Accepted,
            POST_FRAME_WOULD_BLOCK => Self::WouldBlock,
            POST_FRAME_CLOSED => Self::Closed,
            other => Self::Unknown(other),
        }
    }
}

/// Result of a host-to-plugin callback.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CallOutcome {
    Ok,
    InvalidArgument,
    Closed,
    Failed,
    Panicked,
    Unknown(u32),
}

impl CallOutcome {
    pub const fn from_raw(raw: u32) -> Self {
        match raw {
            CALL_OK => Self::Ok,
            CALL_INVALID_ARGUMENT => Self::InvalidArgument,
            CALL_CLOSED => Self::Closed,
            CALL_FAILED => Self::Failed,
            CALL_PANICKED => Self::Panicked,
            other => Self::Unknown(other),
        }
    }
}

/// Protocol identifier for host/plugin frames carried by the fixed-layout ABI.
pub const FRAME_PROTOCOL: &str = "rsi-meta.plugin";
/// Current host/plugin frame version.
pub const FRAME_VERSION: u32 = 0;
/// Maximum Unicode scalar count for identifiers in one plugin frame.
pub const MAX_FRAME_ID_CHARACTERS: usize = 255;

pub const OP_OPEN: &str = "open";
pub const OP_DATA: &str = "data";
pub const OP_CREDIT: &str = "credit";
pub const OP_HALF_CLOSE: &str = "half_close";
pub const OP_CANCEL: &str = "cancel";

pub const EVENT_DATA: &str = "data";
pub const EVENT_CREDIT: &str = "credit";
pub const EVENT_END: &str = "end";
pub const EVENT_CANCEL: &str = "cancel";

pub const RUNTIME_TICK_SERVICE: &str = "runtime.tick";
pub const RUNTIME_TICK_EVENT: &str = "tick";

pub const STATE_OP_GET: &str = "get";
pub const STATE_OP_COMPARE_AND_SWAP: &str = "compare_and_swap";
pub const STATE_OP_DELETE: &str = "delete";

pub const STATE_EVENT_VALUE: &str = "value";
pub const STATE_EVENT_APPLIED: &str = "applied";
pub const STATE_EVENT_CONFLICT: &str = "conflict";
pub const STATE_EVENT_DELETED: &str = "deleted";

const DATA_FRAME_MAGIC: &[u8; 4] = b"RMD0";
const DATA_REQUEST: u8 = 0;
const DATA_EVENT: u8 = 1;
const DATA_HEADER_BYTES: usize = DATA_FRAME_MAGIC.len() + 1 + 2 + 2 + 4;

/// Canonical validated host/plugin wire frame.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Frame {
    pub protocol: String,
    pub version: u32,
    #[serde(flatten)]
    pub body: FrameBody,
}

impl Frame {
    pub fn new(body: FrameBody) -> Self {
        Self {
            protocol: FRAME_PROTOCOL.to_owned(),
            version: FRAME_VERSION,
            body,
        }
    }

    pub fn lifecycle(phase: LifecyclePhase, generation: u64, config: Option<Value>) -> Self {
        Self::new(FrameBody::Lifecycle {
            phase,
            generation,
            config,
        })
    }

    pub fn service_request(
        request_id: impl Into<String>,
        service: impl Into<String>,
        operation: impl Into<String>,
        payload: Value,
    ) -> Self {
        Self::new(FrameBody::ServiceRequest {
            request_id: request_id.into(),
            service: service.into(),
            operation: operation.into(),
            payload,
        })
    }

    pub fn service_event(
        request_id: Option<String>,
        service: impl Into<String>,
        event: impl Into<String>,
        payload: Value,
    ) -> Self {
        Self::new(FrameBody::ServiceEvent {
            request_id,
            service: service.into(),
            event: event.into(),
            payload,
        })
    }

    pub fn durable_command(command_id: impl Into<String>, command: DurableCommand) -> Self {
        Self::new(FrameBody::DurableCommand {
            command_id: command_id.into(),
            command,
        })
    }

    pub fn service_data_request(
        request_id: impl Into<String>,
        service: impl Into<String>,
        payload: impl Into<Vec<u8>>,
    ) -> Self {
        Self::new(FrameBody::ServiceDataRequest {
            request_id: request_id.into(),
            service: service.into(),
            payload: payload.into(),
        })
    }

    pub fn service_data_event(
        request_id: impl Into<String>,
        service: impl Into<String>,
        payload: impl Into<Vec<u8>>,
    ) -> Self {
        Self::new(FrameBody::ServiceDataEvent {
            request_id: request_id.into(),
            service: service.into(),
            payload: payload.into(),
        })
    }

    /// Encodes the validated frame as JSON for the control lane.
    ///
    /// # Errors
    ///
    /// Returns a serialization error if a payload cannot be represented.
    pub fn encode(&self) -> Result<Vec<u8>, FrameError> {
        if self.protocol != FRAME_PROTOCOL {
            return Err(FrameError::UnsupportedProtocol {
                found: self.protocol.clone(),
            });
        }
        if self.version != FRAME_VERSION {
            return Err(FrameError::UnsupportedVersion {
                found: self.version,
            });
        }
        self.body.validate()?;
        match &self.body {
            FrameBody::ServiceDataRequest {
                request_id,
                service,
                payload,
            } => encode_data_frame(DATA_REQUEST, request_id, service, payload),
            FrameBody::ServiceDataEvent {
                request_id,
                service,
                payload,
            } => encode_data_frame(DATA_EVENT, request_id, service, payload),
            _ => serde_json::to_vec(self).map_err(FrameError::Json),
        }
    }

    /// Decodes and validates an untrusted plugin frame.
    ///
    /// # Errors
    ///
    /// Returns a protocol, version, JSON, or field-bound error.
    pub fn decode(bytes: &[u8]) -> Result<Self, FrameError> {
        if bytes.starts_with(DATA_FRAME_MAGIC) {
            return decode_data_frame(bytes);
        }
        let frame: Self = serde_json::from_slice(bytes).map_err(FrameError::Json)?;
        if frame.protocol != FRAME_PROTOCOL {
            return Err(FrameError::UnsupportedProtocol {
                found: frame.protocol,
            });
        }
        if frame.version != FRAME_VERSION {
            return Err(FrameError::UnsupportedVersion {
                found: frame.version,
            });
        }
        frame.body.validate()?;
        Ok(frame)
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum FrameBody {
    Lifecycle {
        phase: LifecyclePhase,
        generation: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        config: Option<Value>,
    },
    ServiceRequest {
        request_id: String,
        service: String,
        operation: String,
        payload: Value,
    },
    ServiceEvent {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        request_id: Option<String>,
        service: String,
        event: String,
        payload: Value,
    },
    #[serde(skip)]
    ServiceDataRequest {
        request_id: String,
        service: String,
        payload: Vec<u8>,
    },
    #[serde(skip)]
    ServiceDataEvent {
        request_id: String,
        service: String,
        payload: Vec<u8>,
    },
    DurableCommand {
        command_id: String,
        command: DurableCommand,
    },
}

impl FrameBody {
    fn validate(&self) -> Result<(), FrameError> {
        match self {
            Self::Lifecycle {
                phase,
                generation,
                config,
            } => {
                if *generation == 0 {
                    return Err(invalid_frame("lifecycle generation must be at least one"));
                }
                match phase {
                    LifecyclePhase::Prepare => {}
                    LifecyclePhase::PrepareFailed => validate_prepare_failure(config.as_ref())?,
                    LifecyclePhase::Prepared
                    | LifecyclePhase::Abort
                    | LifecyclePhase::Committed
                    | LifecyclePhase::Retire
                    | LifecyclePhase::Retired => {
                        if config.is_some() {
                            return Err(invalid_frame(
                                "this lifecycle phase must not carry config",
                            ));
                        }
                    }
                }
            }
            Self::ServiceRequest {
                request_id,
                service,
                operation,
                ..
            } => {
                require_bounded("service request id", request_id)?;
                require_bounded("service request contract", service)?;
                require_bounded("service request operation", operation)?;
                if operation == OP_DATA {
                    return Err(invalid_frame(
                        "service DATA requests require the binary frame variant",
                    ));
                }
            }
            Self::ServiceEvent {
                request_id,
                service,
                event,
                ..
            } => {
                if let Some(request_id) = request_id {
                    require_bounded("service event request id", request_id)?;
                }
                require_bounded("service event contract", service)?;
                require_bounded("service event name", event)?;
                if event == EVENT_DATA {
                    return Err(invalid_frame(
                        "service DATA events require the binary frame variant",
                    ));
                }
            }
            Self::ServiceDataRequest {
                request_id,
                service,
                ..
            } => {
                require_bounded("service request id", request_id)?;
                require_bounded("service request contract", service)?;
            }
            Self::ServiceDataEvent {
                request_id,
                service,
                ..
            } => {
                require_bounded("service event request id", request_id)?;
                require_bounded("service event contract", service)?;
            }
            Self::DurableCommand { command_id, .. } => {
                require_bounded("durable command id", command_id)?;
            }
        }
        Ok(())
    }
}

fn encode_data_frame(
    kind: u8,
    request_id: &str,
    service: &str,
    payload: &[u8],
) -> Result<Vec<u8>, FrameError> {
    require_bounded("service data request id", request_id)?;
    require_bounded("service data contract", service)?;
    let request_len = u16::try_from(request_id.len())
        .map_err(|_| invalid_frame("service data request id is too large"))?;
    let service_len = u16::try_from(service.len())
        .map_err(|_| invalid_frame("service data contract is too large"))?;
    let payload_len = u32::try_from(payload.len())
        .map_err(|_| invalid_frame("service data payload is too large"))?;
    let capacity = DATA_HEADER_BYTES
        .checked_add(request_id.len())
        .and_then(|bytes| bytes.checked_add(service.len()))
        .and_then(|bytes| bytes.checked_add(payload.len()))
        .ok_or_else(|| invalid_frame("service data frame size overflow"))?;
    let mut encoded = Vec::with_capacity(capacity);
    encoded.extend_from_slice(DATA_FRAME_MAGIC);
    encoded.push(kind);
    encoded.extend_from_slice(&request_len.to_be_bytes());
    encoded.extend_from_slice(&service_len.to_be_bytes());
    encoded.extend_from_slice(&payload_len.to_be_bytes());
    encoded.extend_from_slice(request_id.as_bytes());
    encoded.extend_from_slice(service.as_bytes());
    encoded.extend_from_slice(payload);
    Ok(encoded)
}

fn decode_data_frame(bytes: &[u8]) -> Result<Frame, FrameError> {
    if bytes.len() < DATA_HEADER_BYTES {
        return Err(invalid_frame("truncated service data frame header"));
    }
    let kind = bytes[4];
    let request_len = usize::from(u16::from_be_bytes([bytes[5], bytes[6]]));
    let service_len = usize::from(u16::from_be_bytes([bytes[7], bytes[8]]));
    let payload_len_u32 = u32::from_be_bytes([bytes[9], bytes[10], bytes[11], bytes[12]]);
    let payload_len = usize::try_from(payload_len_u32)
        .map_err(|_| invalid_frame("service data payload length is unsupported"))?;
    let request_end = DATA_HEADER_BYTES
        .checked_add(request_len)
        .ok_or_else(|| invalid_frame("service data frame size overflow"))?;
    let service_end = request_end
        .checked_add(service_len)
        .ok_or_else(|| invalid_frame("service data frame size overflow"))?;
    let payload_end = service_end
        .checked_add(payload_len)
        .ok_or_else(|| invalid_frame("service data frame size overflow"))?;
    if payload_end != bytes.len() {
        return Err(invalid_frame(
            "service data frame length does not match header",
        ));
    }
    let request_id = std::str::from_utf8(&bytes[DATA_HEADER_BYTES..request_end])
        .map_err(|_| invalid_frame("service data request id is not UTF-8"))?
        .to_owned();
    let service = std::str::from_utf8(&bytes[request_end..service_end])
        .map_err(|_| invalid_frame("service data contract is not UTF-8"))?
        .to_owned();
    let payload = bytes[service_end..payload_end].to_vec();
    let body = match kind {
        DATA_REQUEST => FrameBody::ServiceDataRequest {
            request_id,
            service,
            payload,
        },
        DATA_EVENT => FrameBody::ServiceDataEvent {
            request_id,
            service,
            payload,
        },
        _ => return Err(invalid_frame("unknown service data frame kind")),
    };
    body.validate()?;
    Ok(Frame::new(body))
}

fn validate_prepare_failure(config: Option<&Value>) -> Result<(), FrameError> {
    let object = config
        .and_then(Value::as_object)
        .ok_or_else(|| invalid_frame("prepare_failed requires an object config"))?;
    if object.keys().any(|key| key != "code" && key != "message") {
        return Err(invalid_frame(
            "prepare_failed config contains an unknown field",
        ));
    }
    let code = object
        .get("code")
        .and_then(Value::as_str)
        .ok_or_else(|| invalid_frame("prepare_failed config requires a string code"))?;
    if code.is_empty()
        || code.len() > 64
        || !code
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
    {
        return Err(invalid_frame(
            "prepare_failed code is outside the v0 schema",
        ));
    }
    if let Some(message) = object.get("message") {
        let message = message
            .as_str()
            .ok_or_else(|| invalid_frame("prepare_failed message must be a string"))?;
        if message.chars().count() > 256
            || message
                .chars()
                .any(|character| character <= '\u{001f}' || character == '\u{007f}')
        {
            return Err(invalid_frame(
                "prepare_failed message is outside the v0 schema",
            ));
        }
    }
    Ok(())
}

fn require_bounded(name: &str, value: &str) -> Result<(), FrameError> {
    if value.is_empty() {
        return Err(invalid_frame(format!("{name} must not be empty")));
    }
    if value.chars().count() > MAX_FRAME_ID_CHARACTERS {
        return Err(invalid_frame(format!(
            "{name} exceeds {MAX_FRAME_ID_CHARACTERS} characters"
        )));
    }
    Ok(())
}

fn invalid_frame(message: impl Into<String>) -> FrameError {
    FrameError::InvalidEnvelope(message.into())
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LifecyclePhase {
    Prepare,
    Prepared,
    PrepareFailed,
    Abort,
    Committed,
    Retire,
    Retired,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum DurableCommand {
    ApplyManifestPath {
        manifest_path: PathBuf,
        lock_path: PathBuf,
    },
}

#[derive(Debug, Error)]
pub enum FrameError {
    #[error("invalid plugin JSON frame: {0}")]
    Json(#[source] serde_json::Error),
    #[error("unsupported plugin frame protocol `{found}`")]
    UnsupportedProtocol { found: String },
    #[error("unsupported plugin frame version {found}")]
    UnsupportedVersion { found: u32 },
    #[error("invalid plugin frame: {0}")]
    InvalidEnvelope(String),
}

/// Safe Rust helpers used by plugin implementations.
pub mod sdk {
    use super::{
        ABI_MAJOR, ABI_MINOR, CALL_CLOSED, CALL_FAILED, CALL_INVALID_ARGUMENT, CALL_OK,
        CALL_PANICKED, HostApi, INIT_INVALID_HOST_API, INIT_OK, INIT_PANICKED, INIT_REJECTED, Lane,
        PluginApi, PostFrameOutcome, c_void, supports_minor,
    };
    use std::fmt;
    use std::panic::{AssertUnwindSafe, catch_unwind};
    use std::sync::Mutex;

    /// Error returned when a host callback is absent or ABI-incompatible.
    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    pub enum HostError {
        IncompatibleApi,
    }

    impl fmt::Display for HostError {
        fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            match self {
                Self::IncompatibleApi => formatter.write_str("incompatible host API"),
            }
        }
    }

    impl std::error::Error for HostError {}

    /// Copy of the validated host table available to safe Rust plugin code.
    #[derive(Clone, Copy)]
    pub struct Host {
        api: HostApi,
    }

    impl fmt::Debug for Host {
        fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter
                .debug_struct("Host")
                .field("abi_major", &self.api.abi_major)
                .field("abi_minor", &self.api.abi_minor)
                .finish_non_exhaustive()
        }
    }

    // SAFETY: `HostApi::new` requires the callback and opaque context to remain
    // valid and callable on every thread used for plugin callbacks. `Host` only
    // copies that table and never dereferences the opaque context itself.
    unsafe impl Send for Host {}
    // SAFETY: The same construction contract requires concurrent host callback
    // invocation to be supported; `Host` contains no Rust references.
    unsafe impl Sync for Host {}

    impl Host {
        fn from_api(api: HostApi) -> Result<Self, HostError> {
            if api.is_compatible() {
                Ok(Self { api })
            } else {
                Err(HostError::IncompatibleApi)
            }
        }

        /// Posts a frame to the host. The host copies `payload` before return.
        ///
        /// # Errors
        ///
        /// Returns an error when the host callback is absent or ABI-incompatible.
        pub fn post_frame(
            &self,
            lane: Lane,
            payload: &[u8],
        ) -> Result<PostFrameOutcome, HostError> {
            let callback = self.api.host_post_frame.ok_or(HostError::IncompatibleApi)?;
            // SAFETY: The constructor contract keeps the callback/context alive.
            // `payload` is readable for its length and the ABI requires the host
            // to copy it synchronously rather than retain the pointer.
            let raw = unsafe {
                callback(
                    self.api.host_handle,
                    lane.as_raw(),
                    payload.as_ptr(),
                    payload.len(),
                )
            };
            Ok(PostFrameOutcome::from_raw(raw))
        }
    }

    /// Safe plugin interface exported by [`crate::export_plugin!`].
    pub trait Plugin: Send + 'static {
        type Error: fmt::Display;

        /// Creates one plugin instance from a validated host table.
        ///
        /// # Errors
        ///
        /// Returns a plugin-defined error when initialization cannot complete.
        fn create(host: Host) -> Result<Self, Self::Error>
        where
            Self: Sized;

        /// Handles one validated lane frame.
        ///
        /// # Errors
        ///
        /// Returns a plugin-defined error when the frame cannot be handled.
        fn on_frame(&mut self, lane: Lane, payload: &[u8]) -> Result<(), Self::Error>;

        /// Stops background work before the host destroys this instance.
        ///
        /// # Errors
        ///
        /// Returns a plugin-defined error when graceful shutdown fails.
        fn shutdown(&mut self) -> Result<(), Self::Error> {
            Ok(())
        }
    }

    #[derive(Debug)]
    struct PluginSlot<P> {
        plugin: Option<P>,
        shutdown: bool,
    }

    #[derive(Debug)]
    struct PluginState<P> {
        slot: Mutex<PluginSlot<P>>,
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct ApiHeader {
        abi_major: u32,
        abi_minor: u32,
        struct_size: u32,
        reserved: u32,
    }

    unsafe extern "C" fn on_frame<P: Plugin>(
        plugin_handle: *mut c_void,
        lane: u32,
        data_ptr: *const u8,
        data_len: usize,
    ) -> u32 {
        if plugin_handle.is_null() || (data_ptr.is_null() && data_len != 0) {
            return CALL_INVALID_ARGUMENT;
        }
        let Some(lane) = Lane::from_raw(lane) else {
            return CALL_INVALID_ARGUMENT;
        };

        let result = catch_unwind(AssertUnwindSafe(|| {
            // SAFETY: The host may only pass the handle produced by `initialize`
            // and may not call this function after `destroy`.
            let state = unsafe { &*(plugin_handle.cast::<PluginState<P>>()) };
            let payload = if data_len == 0 {
                &[]
            } else {
                // SAFETY: Non-null was checked above and the callback contract
                // requires `data_len` readable bytes for the duration of the call.
                unsafe { core::slice::from_raw_parts(data_ptr, data_len) }
            };
            let Ok(mut slot) = state.slot.lock() else {
                return CALL_PANICKED;
            };
            if slot.shutdown {
                return CALL_CLOSED;
            }
            let Some(plugin) = slot.plugin.as_mut() else {
                return CALL_CLOSED;
            };
            match plugin.on_frame(lane, payload) {
                Ok(()) => CALL_OK,
                Err(_) => CALL_FAILED,
            }
        }));

        result.unwrap_or(CALL_PANICKED)
    }

    unsafe extern "C" fn shutdown<P: Plugin>(plugin_handle: *mut c_void) -> u32 {
        if plugin_handle.is_null() {
            return CALL_INVALID_ARGUMENT;
        }
        catch_unwind(AssertUnwindSafe(|| {
            // SAFETY: The host may only pass a live handle returned by `initialize`.
            let state = unsafe { &*(plugin_handle.cast::<PluginState<P>>()) };
            let Ok(mut slot) = state.slot.lock() else {
                return CALL_PANICKED;
            };
            if slot.shutdown {
                return CALL_OK;
            }
            let Some(plugin) = slot.plugin.as_mut() else {
                return CALL_CLOSED;
            };
            match plugin.shutdown() {
                Ok(()) => {
                    slot.shutdown = true;
                    CALL_OK
                }
                Err(_) => CALL_FAILED,
            }
        }))
        .unwrap_or(CALL_PANICKED)
    }

    unsafe extern "C" fn destroy<P: Plugin>(plugin_handle: *mut c_void) -> u32 {
        if plugin_handle.is_null() {
            return CALL_INVALID_ARGUMENT;
        }
        catch_unwind(AssertUnwindSafe(|| {
            // SAFETY: The ABI requires exactly one destroy call for the handle
            // allocated by `initialize`; this reconstructs that Box once.
            let state = unsafe { Box::from_raw(plugin_handle.cast::<PluginState<P>>()) };
            let mut slot = match state.slot.into_inner() {
                Ok(slot) => slot,
                Err(poisoned) => poisoned.into_inner(),
            };
            let mut status = CALL_OK;
            if let Some(mut plugin) = slot.plugin.take() {
                if !slot.shutdown && plugin.shutdown().is_err() {
                    status = CALL_FAILED;
                }
                drop(plugin);
            }
            status
        }))
        .unwrap_or(CALL_PANICKED)
    }

    /// Initializes a plugin and writes its raw function table.
    ///
    /// This is public only so [`crate::export_plugin!`] can invoke it from a
    /// downstream crate.
    ///
    /// # Safety
    ///
    /// `host_api` must point to an aligned, readable common four-`u32` API header
    /// and, when its advertised `struct_size` is large enough, to that many
    /// readable bytes. `plugin_api_out` must point to writable, properly aligned
    /// storage for one [`PluginApi`]. The host must honor the resulting handle's
    /// callback and single-destroy lifecycle.
    #[doc(hidden)]
    pub unsafe fn initialize<P: Plugin>(
        host_api: *const HostApi,
        plugin_api_out: *mut PluginApi,
        plugin_api_capacity: usize,
    ) -> u32 {
        if host_api.is_null()
            || plugin_api_out.is_null()
            || plugin_api_capacity < core::mem::size_of::<PluginApi>()
        {
            return INIT_INVALID_HOST_API;
        }

        // SAFETY: The contract guarantees a readable, aligned common header.
        // Reading this prefix before the full table avoids touching fields that
        // an older or malformed caller did not advertise.
        let header = unsafe { host_api.cast::<ApiHeader>().read() };
        if header.abi_major != ABI_MAJOR
            || !supports_minor(header.abi_minor, ABI_MINOR)
            || header.struct_size < HostApi::STRUCT_SIZE
        {
            return INIT_INVALID_HOST_API;
        }

        // SAFETY: The header now advertises the complete known table, and the
        // function contract requires that many bytes to be readable.
        let host_api = unsafe { host_api.read() };
        let Ok(host) = Host::from_api(host_api) else {
            return INIT_INVALID_HOST_API;
        };

        let plugin = match catch_unwind(AssertUnwindSafe(|| P::create(host))) {
            Ok(Ok(plugin)) => plugin,
            Ok(Err(_)) => return INIT_REJECTED,
            Err(_) => return INIT_PANICKED,
        };
        let state = Box::new(PluginState {
            slot: Mutex::new(PluginSlot {
                plugin: Some(plugin),
                shutdown: false,
            }),
        });
        let table = PluginApi {
            abi_major: ABI_MAJOR,
            abi_minor: ABI_MINOR,
            struct_size: PluginApi::STRUCT_SIZE,
            reserved: 0,
            plugin_handle: Box::into_raw(state).cast(),
            on_frame: Some(on_frame::<P>),
            shutdown: Some(shutdown::<P>),
            destroy: Some(destroy::<P>),
        };
        // SAFETY: Writable output storage is required by this function's contract.
        unsafe { plugin_api_out.write(table) };
        INIT_OK
    }
}

/// Exports a safe Rust [`sdk::Plugin`] as the fixed C entry symbol.
///
/// The generated trampolines catch every unwind from plugin construction and
/// callbacks. With `panic = "abort"`, a panic terminates the process instead;
/// it still never unwinds across the ABI.
#[macro_export]
macro_rules! export_plugin {
    ($plugin:ty) => {
        #[unsafe(no_mangle)]
        pub unsafe extern "C" fn rsi_meta_plugin_entry_v0(
            host_api: *const $crate::HostApi,
            plugin_api_out: *mut $crate::PluginApi,
            plugin_api_capacity: usize,
        ) -> u32 {
            // SAFETY: The loader is responsible for the entry-point pointer
            // contract; the SDK validates all representable metadata before use.
            unsafe {
                $crate::sdk::initialize::<$plugin>(host_api, plugin_api_out, plugin_api_capacity)
            }
        }
    };
}
