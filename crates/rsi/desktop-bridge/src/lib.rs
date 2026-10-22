//! Narrow native conveniences for the ordinary Web document.
#![deny(unsafe_code)]
#![warn(missing_docs)]

use rsi_api_protocol::ProtocolDescriptor;
use rsi_session_protocol::export::ExportEvent;
use serde::{Deserialize, Serialize};

/// Includes worst-case JSON escaping for one 64 KiB export chunk.
pub const MAX_REQUEST: usize = 512 * 1024;
/// Maximum UTF-8 clipboard content.
pub const MAX_CLIPBOARD: usize = 64 * 1024;

/// Exact interface offered by this native bridge.
pub fn protocol_descriptor() -> ProtocolDescriptor {
    rsi_api_protocol::wire_contract!()
}

/// Descriptor-fenced document request.
#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    /// Required interface, independent of native executable identity.
    pub protocol: ProtocolDescriptor,
    /// Explicit native operation.
    pub operation: Operation,
}

/// No variant accepts a native destination path.
#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Operation {
    /// Checks compatibility before native document actions.
    Hello {},
    /// Begins the same draft-draining close path as the window manager.
    RequestClose {},
    /// Writes bounded UTF-8 text; never reads the clipboard.
    Clipboard {
        /// Text to copy.
        text: String,
    },
    /// Reserves a save dialog and verified streaming sink.
    ExportOpen {
        /// Validated Start event supplies the filename hint.
        event: ExportEvent,
    },
    /// Delivers one event with backpressure.
    ExportWrite {
        /// Native reservation.
        token: String,
        /// Existing export framing.
        event: ExportEvent,
    },
    /// Declares EOF and awaits the actual atomic persistence result.
    ExportFinish {
        /// Native reservation.
        token: String,
    },
    /// Cancels a reservation before commit admission.
    ExportCancel {
        /// Native reservation.
        token: String,
    },
    /// Completes or cancels the currently pending close attempt.
    Close {
        /// Exact native close attempt.
        attempt: u64,
        /// True only after draft and Worker disposal completed.
        ready: bool,
    },
}

/// Bounds bytes and checks the exact interface before an operation is admitted.
///
/// # Errors
/// Returns a non-secret categorical validation failure.
pub fn decode(source: &str) -> Result<Operation, &'static str> {
    if source.len() > MAX_REQUEST {
        return Err("Desktop request exceeds its bound");
    }
    let request: Request = serde_json::from_str(source).map_err(|_| "Invalid Desktop request")?;
    if request.protocol != protocol_descriptor() {
        return Err("Desktop bridge interface mismatch");
    }
    match &request.operation {
        Operation::Clipboard { text } if text.len() > MAX_CLIPBOARD => {
            Err("Clipboard text exceeds 64 KiB")
        }
        Operation::ExportWrite { token, .. }
        | Operation::ExportFinish { token }
        | Operation::ExportCancel { token }
            if token.len() != 64 || !token.bytes().all(|c| c.is_ascii_hexdigit()) =>
        {
            Err("Invalid native export token")
        }
        _ => Ok(request.operation),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rejects_unknown_fields_and_interface_drift_before_operations() {
        let mut request = serde_json::to_value(Request {
            protocol: protocol_descriptor(),
            operation: Operation::Hello {},
        })
        .unwrap();
        request["operation"]["extra"] = true.into();
        assert!(decode(&request.to_string()).is_err());
        request["operation"] = serde_json::json!({"kind":"hello"});
        request["protocol"]["version"] = 999.into();
        assert!(decode(&request.to_string()).is_err());
        assert!(decode(&" ".repeat(MAX_REQUEST + 1)).is_err());
    }
}
