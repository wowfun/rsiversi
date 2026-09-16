use crate::error::{McpError, Result};
use rsi_mcp_protocol::MAXIMUM_FRAME_BYTES;
use serde::Serialize;
use serde_json::Value;
use std::io::{self, Write};
struct Limited(Vec<u8>);
impl Write for Limited {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.len() > MAXIMUM_FRAME_BYTES.saturating_sub(self.0.len()) {
            return Err(io::Error::other("MCP frame limit"));
        }
        self.0.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
pub(super) fn encode(value: &impl Serialize) -> Result<Vec<u8>> {
    let mut bytes = Limited(Vec::new());
    serde_json::to_writer(&mut bytes, value).map_err(|_| McpError::Capacity)?;
    Ok(bytes.0)
}
pub(super) fn parse(bytes: &[u8]) -> Result<Value> {
    if bytes.len() > MAXIMUM_FRAME_BYTES {
        return Err(McpError::Capacity);
    }
    let value: Value = serde_json::from_slice(bytes).map_err(|_| McpError::Protocol)?;
    if !value.is_object() || value.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
        return Err(McpError::Protocol);
    }
    if let Some(method) = value.get("method") {
        if method
            .as_str()
            .is_none_or(|name| name.is_empty() || name.len() > 256)
            || value.get("result").is_some()
            || value.get("error").is_some()
            || value
                .get("params")
                .is_some_and(|params| !params.is_object())
        {
            return Err(McpError::Protocol);
        }
    } else if value.get("id").is_none_or(Value::is_null)
        || value.get("result").is_some() == value.get("error").is_some()
    {
        return Err(McpError::Protocol);
    }
    Ok(value)
}
pub(super) fn changed(value: &Value) -> bool {
    matches!(
        value.get("method").and_then(Value::as_str),
        Some("notifications/tools/list_changed" | "notifications/resources/list_changed")
    )
}
pub(super) fn result(mut value: Value, id: &str) -> Result<Value> {
    if value.get("id").and_then(Value::as_str) != Some(id) || value.get("method").is_some() {
        return Err(McpError::Protocol);
    }
    if let Some(error) = value.get("error") {
        return Err(remote_error(error)?);
    }
    value
        .as_object_mut()
        .and_then(|object| object.remove("result"))
        .ok_or(McpError::Protocol)
}
pub(super) fn remote_error(error: &Value) -> Result<McpError> {
    if error.get("message").is_none_or(|v| !v.is_string()) {
        return Err(McpError::Protocol);
    }
    Ok(
        match error
            .get("code")
            .and_then(Value::as_i64)
            .ok_or(McpError::Protocol)?
        {
            -32020 => McpError::HeaderMismatch,
            -32021 => McpError::RequiredCapability,
            -32022 => McpError::UnsupportedVersion,
            _ => McpError::RemoteError,
        },
    )
}
/// A bounded SSE parser. Both the current line and accumulated data obey frame limits.
#[derive(Default)]
pub(super) struct Events {
    line: Vec<u8>,
    data: Vec<u8>,
}
impl Events {
    pub(super) fn feed(&mut self, bytes: &[u8]) -> Result<Vec<Value>> {
        if bytes.len() > MAXIMUM_FRAME_BYTES {
            return Err(McpError::Capacity);
        }
        let mut messages = Vec::new();
        for &byte in bytes {
            if byte != b'\n' {
                if self.line.len() == MAXIMUM_FRAME_BYTES {
                    return Err(McpError::Capacity);
                }
                self.line.push(byte);
                continue;
            }
            if self.line.last() == Some(&b'\r') {
                self.line.pop();
            }
            if self.line.is_empty() {
                if !self.data.is_empty() {
                    self.data.pop();
                    messages.push(parse(&self.data)?);
                    self.data.clear();
                    if messages.len() > 64 {
                        return Err(McpError::Capacity);
                    }
                }
            } else if self.line.starts_with(b"data:") {
                let mut data = &self.line[5..];
                if data.first() == Some(&b' ') {
                    data = &data[1..];
                }
                if data.len() + 1 > MAXIMUM_FRAME_BYTES.saturating_sub(self.data.len()) {
                    return Err(McpError::Capacity);
                }
                self.data.extend_from_slice(data);
                self.data.push(b'\n');
            }
            self.line.clear();
        }
        Ok(messages)
    }
    #[cfg(test)]
    pub(super) fn complete(&self) -> bool {
        self.line.is_empty() && self.data.is_empty()
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn framing_is_lossless_across_utf8_boundaries_and_rejects_oversize_before_parse() {
        let raw = "event: message\r\ndata: {\"jsonrpc\":\"2.0\",\"id\":\"1\",\"result\":\"中文\"}\r\n\r\n";
        let mut parser = Events::default();
        let mut messages = vec![];
        for byte in raw.as_bytes() {
            messages.extend(parser.feed(&[*byte]).unwrap());
        }
        assert_eq!(result(messages.pop().unwrap(), "1").unwrap(), "中文");
        assert!(parser.complete());
        assert_eq!(
            encode(&"x".repeat(MAXIMUM_FRAME_BYTES)),
            Err(McpError::Capacity)
        );
        assert_eq!(
            parser.feed(&vec![b'x'; MAXIMUM_FRAME_BYTES + 1]),
            Err(McpError::Capacity)
        );
        assert!(parse(b"[]").is_err());
        assert!(parse(br#"{"jsonrpc":"2.0","id":"x","result":null,"error":{}}"#).is_err());
    }
}
