use crate::{Config, Error, Operation, Position, Result};
use rsi_process::ManagedDuplexProcess;
use serde_json::{Value, json};

const HEADER_BYTES: usize = 8192;
const MESSAGE_BYTES: usize = 1024 * 1024;
mod pump;

/// Query-side handle; the retained pump owns byte progress and retirement.
#[derive(Debug)]
pub(crate) struct Connection {
    wire: pump::Wire,
    capabilities: Value,
    version: i32,
}
impl Connection {
    pub fn new(
        process: ManagedDuplexProcess,
        config: &Config,
        execution: &rsi_meta::Execution,
        stop: tokio_util::sync::CancellationToken,
    ) -> Self {
        Self {
            wire: pump::Wire::new(process, config.configuration.clone(), execution, stop),
            capabilities: Value::Null,
            version: 0,
        }
    }
    pub fn failed(&self) -> bool {
        self.wire.failed()
    }
    pub async fn begin(&self, deadline: tokio::time::Instant) -> Result<()> {
        self.wire
            .call(pump::Action::Begin(deadline))
            .await
            .map(|_| ())
    }
    pub async fn end(&self) -> Result<()> {
        self.wire.call(pump::Action::End).await.map(|_| ())
    }
    async fn request(&self, method: &str, params: Value) -> Result<Value> {
        self.wire
            .call(pump::Action::Send {
                method: method.into(),
                params,
                request: true,
            })
            .await
    }
    async fn notify(&self, method: &str, params: Value) -> Result<()> {
        self.wire
            .call(pump::Action::Send {
                method: method.into(),
                params,
                request: false,
            })
            .await
            .map(|_| ())
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
    pub async fn close(&mut self) -> Result<()> {
        self.wire.close().await
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
