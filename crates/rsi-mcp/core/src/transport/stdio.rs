use super::{State, http::valid_server_id, wire};
use crate::error::{McpError, Result};
use rsi_credentials_protocol::CredentialsResolve;
use rsi_mcp_protocol::{EnvironmentValue, MAXIMUM_FRAME_BYTES};
use rsi_process::{
    DuplexProcess, DuplexProcessSpec, MAXIMUM_DUPLEX_CHUNK_BYTES, ManagedDuplexProcess,
};
use rsi_sandbox::{ProcessRequest, Sandbox, SandboxMode};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    path::Path,
    sync::{Arc, Mutex},
};
use tokio::{
    sync::{Mutex as AsyncMutex, mpsc},
    task::JoinHandle,
};
pub(super) struct Stdio {
    process: ManagedDuplexProcess,
    writer: Arc<AsyncMutex<()>>,
    responses: AsyncMutex<mpsc::Receiver<Value>>,
    reader: Mutex<Option<JoinHandle<()>>>,
    responder: Mutex<Option<JoinHandle<()>>>,
    state: Arc<State>,
}
struct WriteGuard<'a> {
    process: &'a ManagedDuplexProcess,
    state: &'a State,
    settled: bool,
}
impl Drop for WriteGuard<'_> {
    fn drop(&mut self) {
        if !self.settled {
            self.state.invalidate();
            self.process.terminate();
        }
    }
}
async fn write(
    process: &ManagedDuplexProcess,
    state: &State,
    writer: &AsyncMutex<()>,
    mut bytes: Vec<u8>,
) -> Result<()> {
    let _lock = writer.lock().await;
    if !state.valid() {
        return Err(McpError::Disconnected);
    }
    let mut guard = WriteGuard {
        process,
        state,
        settled: false,
    };
    bytes.push(b'\n');
    for chunk in bytes.chunks(MAXIMUM_DUPLEX_CHUNK_BYTES) {
        let mut offset = 0;
        while offset < chunk.len() {
            let written = process
                .stdin()
                .write(&chunk[offset..])
                .await
                .map_err(|_| McpError::Disconnected)?;
            if written == 0 || written > chunk.len() - offset {
                return Err(McpError::Protocol);
            }
            offset += written;
        }
    }
    guard.settled = true;
    Ok(())
}
fn respond(
    process: ManagedDuplexProcess,
    state: Arc<State>,
    writer: Arc<AsyncMutex<()>>,
    mut replies: mpsc::Receiver<Vec<u8>>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let pump = async {
            while let Some(reply) = replies.recv().await {
                write(&process, &state, &writer, reply).await?;
            }
            Ok::<_, McpError>(())
        };
        tokio::select! {biased;
            () = state.stop.cancelled() => {},
            result = pump => { if let Err(error) = result { state.fail(error); } },
        }
        state.invalidate();
        process.terminate();
    })
}
async fn resolve_environment(
    environment: &BTreeMap<String, EnvironmentValue>,
    credentials: &dyn CredentialsResolve,
) -> Result<Vec<(std::ffi::OsString, std::ffi::OsString)>> {
    let mut values = Vec::new();
    for (key, value) in environment {
        let value = match value {
            EnvironmentValue::Literal { value } => value.clone(),
            EnvironmentValue::Credential { reference } => credentials
                .resolve(reference)
                .await
                .map_err(|_| McpError::CredentialUnavailable)?
                .secret
                .expose_secret()
                .to_owned(),
        };
        values.push((key.into(), value.into()));
    }
    Ok(values)
}
impl Stdio {
    #[allow(clippy::too_many_arguments)]
    pub async fn spawn(
        program: &Path,
        arguments: &[String],
        cwd: &Path,
        environment: &BTreeMap<String, EnvironmentValue>,
        credentials: &dyn CredentialsResolve,
        processes: &dyn DuplexProcess,
        sandbox: &dyn Sandbox,
        state: Arc<State>,
    ) -> Result<Self> {
        let confined = sandbox
            .confine(ProcessRequest {
                stdio: rsi_sandbox::ProcessStdio::Pipes,
                mode: SandboxMode::DangerFullAccess,
                program: program.to_owned(),
                arguments: arguments.to_vec(),
                cwd: cwd.to_owned(),
                workspace: cwd.to_owned(),
            })
            .await
            .map_err(|_| McpError::ProcessUnavailable)?;
        let values = resolve_environment(environment, credentials).await?;
        let process = processes
            .spawn(DuplexProcessSpec {
                process: confined,
                environment: values,
                stdout_buffer_bytes: 64 * 1024,
                stderr_max_bytes: 16 * 1024,
                termination_grace_ms: 1000,
            })
            .map_err(|_| McpError::ProcessUnavailable)?;
        let writer = Arc::new(AsyncMutex::new(()));
        let (sender, receiver) = mpsc::channel(1);
        let (replies, reply_queue) = mpsc::channel::<Vec<u8>>(1);
        let responder = respond(process.clone(), state.clone(), writer.clone(), reply_queue);
        let child = process.clone();
        let shared = state.clone();
        let reader = tokio::spawn(async move {
            let pump = async {
                let output = child.stdout();
                let mut line = Vec::new();
                loop {
                    let chunk = output
                        .read(8192)
                        .await
                        .map_err(|_| McpError::Disconnected)?;
                    for byte in chunk.bytes {
                        if byte != b'\n' {
                            if line.len() == MAXIMUM_FRAME_BYTES {
                                return Err(McpError::Capacity);
                            }
                            line.push(byte);
                            continue;
                        }
                        if line.last() == Some(&b'\r') {
                            line.pop();
                        }
                        let value = wire::parse(&line)?;
                        line.clear();
                        if shared.modern() && shared.subscription_message(&value, false)? {
                            continue;
                        }
                        if !shared.modern() && wire::changed(&value) {
                            return Err(McpError::CatalogChanged);
                        }
                        if let Some(method) = value.get("method").and_then(Value::as_str) {
                            if let Some(id) = value.get("id") {
                                if shared.modern() || !valid_server_id(id) {
                                    return Err(McpError::Protocol);
                                }
                                let reply = if method == "ping" {
                                    json!({"jsonrpc":"2.0","id":id,"result":{}})
                                } else {
                                    json!({"jsonrpc":"2.0","id":id,"error":{"code":-32601,"message":"Client capability is not available"}})
                                };
                                replies
                                    .try_send(wire::encode(&reply)?)
                                    .map_err(|_| McpError::Capacity)?;
                            }
                        } else {
                            sender.try_send(value).map_err(|_| McpError::Protocol)?;
                        }
                    }
                    if chunk.eof {
                        return Err::<(), _>(McpError::Disconnected);
                    }
                }
            };
            tokio::select! { () = shared.stop.cancelled() => {}, result = pump => { if let Err(error) = result { shared.fail(error); } } }
            shared.invalidate();
            child.terminate();
        });
        Ok(Self {
            process,
            writer,
            responses: AsyncMutex::new(receiver),
            reader: Mutex::new(Some(reader)),
            responder: Mutex::new(Some(responder)),
            state,
        })
    }
    pub async fn exchange(&self, bytes: Vec<u8>, id: Option<&str>) -> Result<Option<Value>> {
        write(&self.process, &self.state, &self.writer, bytes).await?;
        let Some(id) = id else {
            return Ok(None);
        };
        let value = self
            .responses
            .lock()
            .await
            .recv()
            .await
            .ok_or_else(|| self.state.failure().unwrap_or(McpError::Disconnected))?;
        wire::result(value, id).map(Some)
    }
    pub fn close(&self) {
        self.state.invalidate();
        self.process.terminate();
    }
    pub async fn shutdown(&self) -> Result<()> {
        self.close();
        let reader = self.reader.lock().expect("MCP reader poisoned").take();
        if let Some(reader) = reader {
            let _ = reader.await;
        }
        let responder = self
            .responder
            .lock()
            .expect("MCP responder poisoned")
            .take();
        if let Some(responder) = responder {
            let _ = responder.await;
        }
        self.process
            .wait_settlement()
            .await
            .map_err(|_| McpError::Disconnected)
    }
}
impl Drop for Stdio {
    fn drop(&mut self) {
        self.close();
    }
}
