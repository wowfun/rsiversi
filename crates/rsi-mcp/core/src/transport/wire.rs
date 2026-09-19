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
    has_data: bool,
    named_non_message: bool,
    bom: usize,
    skip_lf: bool,
    work_bytes: usize,
    work_messages: usize,
}
#[derive(Debug)]
pub(super) enum EventStep {
    Message(Value),
    Yield,
    NeedInput,
}
impl Events {
    /// Advances borrowed input without retaining the transport chunk or a message batch.
    pub(super) fn next(&mut self, input: &mut &[u8]) -> Result<EventStep> {
        const BOM: &[u8] = b"\xef\xbb\xbf";
        loop {
            if self.work_bytes >= 16 * 1024 || self.work_messages >= 64 {
                self.work_bytes = 0;
                self.work_messages = 0;
                return Ok(EventStep::Yield);
            }
            let Some((&byte, rest)) = input.split_first() else {
                return Ok(EventStep::NeedInput);
            };
            *input = rest;
            self.work_bytes += 1;
            if self.bom < BOM.len() {
                if byte == BOM[self.bom] {
                    self.bom += 1;
                    continue;
                }
                self.line.extend_from_slice(&BOM[..self.bom]);
                self.bom = BOM.len();
            }
            if self.skip_lf {
                self.skip_lf = false;
                if byte == b'\n' {
                    continue;
                }
            }
            if matches!(byte, b'\r' | b'\n') {
                self.skip_lf = byte == b'\r';
                if let Some(message) = self.end_line()? {
                    self.work_messages += 1;
                    return Ok(EventStep::Message(message));
                }
                continue;
            }
            if self.line.len() == MAXIMUM_FRAME_BYTES {
                return Err(McpError::Capacity);
            }
            self.line.push(byte);
        }
    }
    fn end_line(&mut self) -> Result<Option<Value>> {
        if self.line.is_empty() {
            let message = if self.has_data && !self.named_non_message {
                Some(parse(&self.data)?)
            } else {
                None
            };
            self.data.clear();
            self.has_data = false;
            self.named_non_message = false;
            return Ok(message);
        }
        let mut fields = self.line.splitn(2, |byte| *byte == b':');
        let field = fields.next();
        let value = fields.next().unwrap_or_default();
        let value = value.strip_prefix(b" ").unwrap_or(value);
        if field == Some(b"event".as_slice()) {
            self.named_non_message = !matches!(value, b"" | b"message");
        } else if field == Some(b"data".as_slice()) {
            if value.len() + usize::from(self.has_data)
                > MAXIMUM_FRAME_BYTES.saturating_sub(self.data.len())
            {
                return Err(McpError::Capacity);
            }
            if self.has_data {
                self.data.push(b'\n');
            }
            self.data.extend_from_slice(value);
            self.has_data = true;
        }
        self.line.clear();
        Ok(None)
    }
    #[cfg(test)]
    fn feed(&mut self, mut bytes: &[u8]) -> Result<Vec<Value>> {
        let mut messages = Vec::new();
        loop {
            match self.next(&mut bytes)? {
                EventStep::Message(value) => messages.push(value),
                EventStep::Yield => {}
                EventStep::NeedInput => break,
            }
        }
        Ok(messages)
    }
    #[cfg(test)]
    pub(super) fn complete(&self) -> bool {
        self.line.is_empty() && !self.has_data && !matches!(self.bom, 1 | 2)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn named_events_are_filtered_and_event_metadata_resets_at_every_boundary() {
        let source = b"event: ping\nid: keepalive\nretry: 100\ndata: not-json\n\nevent: ping\n\nevent: ping\nevent: message\ndata: {\"jsonrpc\":\"2.0\",\"id\":\"one\",\"result\":{}}\n\nevent:\ndata: {\"jsonrpc\":\"2.0\",\"id\":\"two\",\"result\":{}}\n\ndata: {\"jsonrpc\":\"2.0\",\"id\":\"three\",\"result\":{}}\n\n";
        for split in 0..=source.len() {
            let mut parser = Events::default();
            let mut messages = parser.feed(&source[..split]).unwrap();
            messages.extend(parser.feed(&source[split..]).unwrap());
            assert_eq!(
                messages
                    .iter()
                    .map(|m| m["id"].as_str().unwrap())
                    .collect::<Vec<_>>(),
                ["one", "two", "three"]
            );
            assert!(parser.complete());
        }
    }
    #[test]
    fn colonless_data_is_an_empty_field_but_still_requires_a_json_message() {
        for ending in ["\n", "\r", "\r\n"] {
            for field in ["data", "data:"] {
                let empty = format!("{field}{ending}{ending}");
                assert!(matches!(
                    Events::default().feed(empty.as_bytes()),
                    Err(McpError::Protocol)
                ));
                let raw = format!(
                    "{field}{ending}data: {{\"jsonrpc\":\"2.0\",\"id\":\"1\",\"result\":\"ok\"}}{ending}{field}{ending}{ending}"
                );
                for cut in 0..=raw.len() {
                    let mut parser = Events::default();
                    let mut messages = parser.feed(&raw.as_bytes()[..cut]).unwrap();
                    messages.extend(parser.feed(&raw.as_bytes()[cut..]).unwrap());
                    assert_eq!(messages.len(), 1);
                    assert_eq!(result(messages.pop().unwrap(), "1").unwrap(), "ok");
                }
            }
        }
    }
    #[test]
    fn legal_sse_line_endings_and_bom_are_partition_invariant() {
        for ending in ["\n", "\r", "\r\n"] {
            for bom in ["", "\u{feff}"] {
                let raw = format!(
                    "{bom}data: {{\"jsonrpc\":\"2.0\",\"id\":\"1\",\"result\":\"中文\"}}{ending}{ending}"
                );
                for cut in 0..=raw.len() {
                    let mut parser = Events::default();
                    let mut messages = parser.feed(&raw.as_bytes()[..cut]).unwrap();
                    messages.extend(parser.feed(&raw.as_bytes()[cut..]).unwrap());
                    assert_eq!(
                        messages.len(),
                        1,
                        "ending={ending:?}, BOM={bom:?}, cut={cut}"
                    );
                    assert_eq!(result(messages.pop().unwrap(), "1").unwrap(), "中文");
                }
            }
        }
    }
    #[test]
    fn notification_bursts_do_not_depend_on_transport_batching() {
        let notification = b"data: {\"jsonrpc\":\"2.0\",\"method\":\"notifications/progress\"}\n\n";
        let mut raw = notification.repeat(65);
        raw.extend_from_slice(b"data: {\"jsonrpc\":\"2.0\",\"id\":\"1\",\"result\":{}}\n\n");
        for cut in 0..=raw.len() {
            let mut parser = Events::default();
            let mut messages = parser.feed(&raw[..cut]).unwrap();
            messages.extend(parser.feed(&raw[cut..]).unwrap());
            assert_eq!(messages.len(), 66, "cut={cut}");
        }
    }
    #[test]
    fn comments_and_messages_yield_across_chunks_without_changing_parser_state() {
        let mut parser = Events::default();
        let comments = b": keepalive\r\n\r\n".repeat(3000);
        let mut consumed = 0;
        let mut yields = 0;
        for byte in &comments {
            let mut input = std::slice::from_ref(byte);
            loop {
                match parser.next(&mut input).unwrap() {
                    EventStep::Yield => {
                        yields += 1;
                        assert!(consumed <= 16 * 1024);
                        consumed = 0;
                    }
                    EventStep::NeedInput => break,
                    EventStep::Message(_) => panic!("comment produced a message"),
                }
            }
            consumed += 1;
        }
        assert!(yields >= 2);
        assert!(parser.complete());
        let notification = b"data: {\"jsonrpc\":\"2.0\",\"method\":\"notice\"}\n\n";
        let raw = notification.repeat(65);
        let mut input = raw.as_slice();
        let mut parser = Events::default();
        for _ in 0..64 {
            assert!(matches!(
                parser.next(&mut input).unwrap(),
                EventStep::Message(_)
            ));
        }
        assert!(matches!(parser.next(&mut input).unwrap(), EventStep::Yield));
        assert!(matches!(
            parser.next(&mut input).unwrap(),
            EventStep::Message(_)
        ));
    }
    #[test]
    fn seeded_multiple_cuts_multiline_data_and_incomplete_eof_preserve_framing() {
        let raw = "\u{feff}: ignored\rdata: {\r\ndata: \"jsonrpc\":\"2.0\",\"id\":\"1\",\"result\":\"中文\"}\r\n\r\n";
        for seed in 1..100_u64 {
            let mut rng = seed;
            let mut offset = 0;
            let mut parser = Events::default();
            let mut values = vec![];
            while offset < raw.len() {
                rng = rng.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
                let end = (offset + 1 + usize::try_from(rng % 17).unwrap()).min(raw.len());
                values.extend(parser.feed(&raw.as_bytes()[offset..end]).unwrap());
                offset = end;
            }
            assert_eq!(values.len(), 1);
            assert_eq!(result(values.pop().unwrap(), "1").unwrap(), "中文");
        }
        for raw in [b"data: {".as_slice(), b"data: {}\n", b"\xef\xbb"] {
            let mut parser = Events::default();
            assert!(parser.feed(raw).unwrap().is_empty());
            assert!(!parser.complete());
        }
        let mut parser = Events::default();
        let mut raw = b"data:".to_vec();
        raw.extend(std::iter::repeat_n(b' ', MAXIMUM_FRAME_BYTES));
        assert!(matches!(parser.feed(&raw), Err(McpError::Capacity)));
    }
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
