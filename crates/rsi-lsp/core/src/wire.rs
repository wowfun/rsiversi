use crate::{Config, Error, Operation, Position, Result};
use rsi_process::ManagedDuplexProcess;
use serde_json::{Value, json};

const HEADER_BYTES: usize = 8192;
const MESSAGE_BYTES: usize = 1024 * 1024;
/// One serialized stream; no detached reader or unbounded pending-request map.
#[derive(Debug)]
pub(crate) struct Connection {
    pub process: ManagedDuplexProcess,
    buffer: Vec<u8>,
    next: u64,
    pub active: Option<u64>,
    pub capabilities: Value,
    configuration: Value,
    incoming: usize,
    server_requests: usize,
    version: i32,
}
impl Connection {
    pub fn new(process: ManagedDuplexProcess, config: &Config) -> Self {
        Self {
            process,
            buffer: vec![],
            next: 0,
            active: None,
            capabilities: Value::Null,
            configuration: config.configuration.clone(),
            incoming: 0,
            server_requests: 0,
            version: 0,
        }
    }
    pub fn reset_budget(&mut self) {
        self.incoming = 0;
        self.server_requests = 0;
    }
    pub async fn send(&self, value: Value) -> Result<()> {
        let body = serde_json::to_vec(&value).map_err(|_| Error::Protocol)?;
        if body.len() > MESSAGE_BYTES {
            return Err(Error::Limit);
        }
        let header = format!("Content-Length: {}\r\n\r\n", body.len());
        let bytes = [header.as_bytes(), &body].concat();
        let input = self.process.stdin();
        let mut offset = 0;
        while offset < bytes.len() {
            let end = (offset + rsi_process::MAXIMUM_DUPLEX_CHUNK_BYTES).min(bytes.len());
            let written = input
                .write(&bytes[offset..end])
                .await
                .map_err(|_| Error::Unavailable)?;
            if written == 0 || written > end - offset {
                return Err(Error::Protocol);
            }
            offset += written;
        }
        Ok(())
    }
    async fn more(&mut self) -> Result<()> {
        let chunk = self
            .process
            .stdout()
            .read(65536)
            .await
            .map_err(|_| Error::Unavailable)?;
        if chunk.bytes.is_empty() {
            return Err(Error::Unavailable);
        }
        self.incoming = self
            .incoming
            .checked_add(chunk.bytes.len())
            .ok_or(Error::Limit)?;
        if self.incoming > 4 * 1024 * 1024
            || self.buffer.len() + chunk.bytes.len() > MESSAGE_BYTES + HEADER_BYTES + 65536
        {
            return Err(Error::Limit);
        }
        self.buffer.extend_from_slice(&chunk.bytes);
        Ok(())
    }
    async fn receive(&mut self) -> Result<Value> {
        let header_end = loop {
            if let Some(end) = self
                .buffer
                .windows(4)
                .position(|window| window == b"\r\n\r\n")
            {
                if end > HEADER_BYTES {
                    return Err(Error::Limit);
                }
                break end;
            }
            if self.buffer.len() > HEADER_BYTES {
                return Err(Error::Limit);
            }
            self.more().await?;
        };
        let length = length(&self.buffer[..header_end])?;
        let end = header_end + 4 + length;
        while self.buffer.len() < end {
            self.more().await?;
        }
        let value = crate::json::decode(&self.buffer[header_end + 4..end])?;
        self.buffer.drain(..end);
        if value.get("jsonrpc").and_then(Value::as_str) != Some("2.0") || !value.is_object() {
            return Err(Error::Protocol);
        }
        Ok(value)
    }
    pub async fn request(&mut self, method: &str, params: Value) -> Result<Value> {
        self.next = self
            .next
            .checked_add(1)
            .filter(|n| i32::try_from(*n).is_ok())
            .ok_or(Error::Limit)?;
        let id = self.next;
        self.active = Some(id);
        self.send(json!({"jsonrpc":"2.0","id":id,"method":method,"params":params}))
            .await?;
        loop {
            let message = self.receive().await?;
            if let Some(method) = message.get("method") {
                let method = method.as_str().ok_or(Error::Protocol)?;
                if message.get("result").is_some() || message.get("error").is_some() {
                    return Err(Error::Protocol);
                }
                if let Some(id) = message.get("id") {
                    self.server_requests += 1;
                    if self.server_requests > 256 {
                        return Err(Error::Limit);
                    }
                    if !valid_id(id) {
                        return Err(Error::Protocol);
                    }
                    let result = match method {
                        "workspace/configuration" => {
                            let items = message
                                .pointer("/params/items")
                                .and_then(Value::as_array)
                                .filter(|v| v.len() <= 16)
                                .ok_or(Error::Protocol)?;
                            Some(Value::Array(
                                items
                                    .iter()
                                    .map(|item| {
                                        item.get("section")
                                            .and_then(Value::as_str)
                                            .filter(|s| s.len() <= 128)
                                            .map_or_else(
                                                || self.configuration.clone(),
                                                |section| {
                                                    section
                                                        .split('.')
                                                        .try_fold(&self.configuration, |v, key| {
                                                            v.get(key)
                                                        })
                                                        .cloned()
                                                        .unwrap_or(Value::Null)
                                                },
                                            )
                                    })
                                    .collect(),
                            ))
                        }
                        "window/workDoneProgress/create" => Some(Value::Null),
                        _ => None,
                    };
                    let reply = match result {
                        Some(result) => json!({"jsonrpc":"2.0","id":id,"result":result}),
                        None => {
                            json!({"jsonrpc":"2.0","id":id,"error":{"code":-32601,"message":"Read-only client method unavailable"}})
                        }
                    };
                    self.send(reply).await?;
                }
                continue;
            }
            if message.get("id").and_then(Value::as_u64) != Some(id)
                || message.get("result").is_some() == message.get("error").is_some()
            {
                return Err(Error::Protocol);
            }
            self.active = None;
            if let Some(error) = message.get("error") {
                let code = error
                    .get("code")
                    .and_then(Value::as_i64)
                    .and_then(|code| i32::try_from(code).ok())
                    .ok_or(Error::Protocol)?;
                if !error.get("message").is_some_and(Value::is_string) {
                    return Err(Error::Protocol);
                }
                return Err(Error::Server(code));
            }
            return Ok(message["result"].clone());
        }
    }
    pub async fn notify(&self, method: &str, params: Value) -> Result<()> {
        self.send(json!({"jsonrpc":"2.0","method":method,"params":params}))
            .await
    }
    pub async fn initialize(&mut self, workspace: &std::path::Path, config: &Config) -> Result<()> {
        let root = url::Url::from_directory_path(workspace)
            .map_err(|()| Error::Invalid)?
            .to_string();
        let result=self.request("initialize",json!({"processId":null,"rootUri":root,"workspaceFolders":[{"uri":root,"name":"workspace"}],"capabilities":{"general":{"positionEncodings":["utf-16"]},"workspace":{"configuration":true,"applyEdit":false,"workspaceFolders":true},"window":{"workDoneProgress":true},"textDocument":{"synchronization":{"dynamicRegistration":false,"didSave":false},"definition":{"dynamicRegistration":false,"linkSupport":true},"references":{"dynamicRegistration":false},"implementation":{"dynamicRegistration":false,"linkSupport":true},"hover":{"dynamicRegistration":false,"contentFormat":["plaintext","markdown"]}}},"initializationOptions":config.initialization_options})).await?;
        let capabilities = result
            .get("capabilities")
            .filter(|v| v.is_object())
            .ok_or(Error::Protocol)?;
        if capabilities
            .get("positionEncoding")
            .is_some_and(|v| v != "utf-16")
        {
            return Err(Error::Unsupported);
        }
        let sync = capabilities
            .get("textDocumentSync")
            .ok_or(Error::Unsupported)?;
        if !(matches!(sync.as_u64(), Some(1 | 2))
            || sync.get("openClose") == Some(&Value::Bool(true)))
        {
            return Err(Error::Unsupported);
        }
        self.capabilities = capabilities.clone();
        self.notify("initialized", json!({})).await
    }
    pub async fn query(
        &mut self,
        operation: Operation,
        uri: &str,
        language: &str,
        text: &str,
        position: Position,
    ) -> Result<Value> {
        if !self
            .capabilities
            .get(operation.capability())
            .is_some_and(|value| value == true || value.is_object())
        {
            return Err(Error::Unsupported);
        }
        self.version = self.version.checked_add(1).ok_or(Error::Limit)?;
        self.notify("textDocument/didOpen",json!({"textDocument":{"uri":uri,"languageId":language,"version":self.version,"text":text}})).await?;
        let mut params = json!({"textDocument":{"uri":uri},"position":position});
        if operation == Operation::References {
            params["context"] = json!({"includeDeclaration":true});
        }
        let result = self.request(operation.method(), params).await;
        let closed = self
            .notify("textDocument/didClose", json!({"textDocument":{"uri":uri}}))
            .await;
        result.and_then(|value| closed.map(|()| value))
    }
    pub async fn close(mut self) -> Result<()> {
        if let Some(id) = self.active {
            let _ = tokio::time::timeout(
                std::time::Duration::from_millis(200),
                self.notify("$/cancelRequest", json!({"id":id})),
            )
            .await;
            let _ = tokio::time::timeout(std::time::Duration::from_millis(200), async {
                loop {
                    let message = self.receive().await?;
                    if message.get("id").and_then(Value::as_u64) == Some(id) {
                        break Ok::<(), Error>(());
                    }
                }
            })
            .await;
        } else if tokio::time::timeout(
            std::time::Duration::from_millis(200),
            self.request("shutdown", Value::Null),
        )
        .await
        .is_ok()
        {
            let _ = tokio::time::timeout(
                std::time::Duration::from_millis(200),
                self.notify("exit", Value::Null),
            )
            .await;
        }
        self.process.terminate();
        self.process
            .wait_settlement()
            .await
            .map_err(|_| Error::Unavailable)
    }
}
fn valid_id(value: &Value) -> bool {
    value.as_i64().is_some() || value.as_str().is_some_and(|s| s.len() <= 128)
}
fn length(header: &[u8]) -> Result<usize> {
    if !header.is_ascii() {
        return Err(Error::Protocol);
    }
    let header = std::str::from_utf8(header).map_err(|_| Error::Protocol)?;
    let mut length = None;
    for line in header.split("\r\n") {
        let (key, value) = line.split_once(':').ok_or(Error::Protocol)?;
        if key.eq_ignore_ascii_case("Content-Length") {
            let value = value.trim();
            if length.is_some()
                || value.is_empty()
                || value.len() > 10
                || !value.bytes().all(|b| b.is_ascii_digit())
            {
                return Err(Error::Protocol);
            }
            let parsed = value.parse::<usize>().map_err(|_| Error::Limit)?;
            if parsed == 0 || parsed > MESSAGE_BYTES {
                return Err(Error::Limit);
            }
            length = Some(parsed);
        } else if !key.eq_ignore_ascii_case("Content-Type") {
            return Err(Error::Protocol);
        }
    }
    length.ok_or(Error::Protocol)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn header_admission_rejects_ambiguity_before_body_allocation() {
        assert_eq!(length(b"Content-Length: 1"), Ok(1));
        assert_eq!(length(b"content-length: 1048576\r\nContent-Type: application/vscode-jsonrpc; charset=utf-8"),Ok(MESSAGE_BYTES));
        for header in [
            b"Content-Length: 1\r\nContent-Length: 1".as_slice(),
            b"Content-Length: +1",
            b"Content-Length: 0",
            b"Content-Length: 1048577",
            b"Content-Length: 1\nWrong: 2",
            b"Content-Type: text/plain",
        ] {
            assert!(length(header).is_err());
        }
    }
}
