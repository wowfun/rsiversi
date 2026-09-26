use async_trait::async_trait;
use futures_util::{FutureExt, StreamExt, stream::FuturesUnordered};
use rsi_jobs::{
    JobControl, JobOutputRead, JobProducer, JobRequest, JobStatus, JobStream, JobTerminal,
    JobsError,
};
use rsi_process::{DuplexInput, DuplexOutput, DuplexProcess, DuplexProcessSpec};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    collections::BTreeSet,
    fmt,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
};
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;

pub const MAXIMUM_FRAME: usize = 1024 * 1024;
use rsi_agent_session_protocol::{
    MAXIMUM_PROGRAM_OUTSTANDING_CALLS, MAXIMUM_PROGRAM_RESULT_BYTES as MAXIMUM_RESULT,
    MAXIMUM_PROGRAM_SCRIPT_BYTES as MAXIMUM_SCRIPT,
};

/// One frozen program's trusted RPC surface, independent of the Node engine.
#[async_trait]
pub trait ProgramRpc: fmt::Debug + Send + Sync + 'static {
    /// Descriptions made available to this exact script.
    fn definitions(&self) -> Value;
    /// Executes an admitted request with cooperative cancellation.
    async fn call(
        &self,
        method: String,
        arguments: Value,
        cancellation: CancellationToken,
    ) -> Result<Value, String>;
}

#[derive(Debug)]
pub(crate) struct Request {
    pub spec: DuplexProcessSpec,
    pub script: String,
    pub rpc: Arc<dyn ProgramRpc>,
    pub start: CancellationToken,
    pub cancel: CancellationToken,
    pub cancelled_at_settlement: AtomicBool,
    pub outcome: watch::Sender<Option<Result<Value, String>>>,
}
#[derive(Debug)]
pub(crate) struct Producer(pub Arc<dyn DuplexProcess>);
impl JobProducer for Producer {
    fn start(&self, request: &JobRequest) -> rsi_jobs::Result<Arc<dyn JobControl>> {
        let request = request
            .downcast_ref::<Arc<Request>>()
            .ok_or_else(|| JobsError::InvalidInput("wrong program producer request".into()))?
            .clone();
        request
            .spec
            .validate()
            .map_err(|error| JobsError::InvalidInput(error.to_string()))?;
        if request.script.len() > MAXIMUM_SCRIPT {
            return Err(JobsError::InvalidInput("script exceeds 64 KiB".into()));
        }
        let control = Arc::new(Control {
            request: request.clone(),
            stderr: Mutex::new(None),
        });
        let process = self.0.clone();
        let task = control.clone();
        tokio::spawn(async move {
            let result = tokio::select! {
                biased;
                () = request.cancel.cancelled() => Err("program cancelled before start".into()),
                () = request.start.cancelled() => execute(process, &request, &task.stderr).await,
            };
            request
                .cancelled_at_settlement
                .store(request.cancel.is_cancelled(), Ordering::Release);
            request.outcome.send_replace(Some(result));
        });
        Ok(control)
    }
}
#[derive(Debug)]
struct Control {
    request: Arc<Request>,
    stderr: Mutex<Option<rsi_process::ProcessRead>>,
}
#[async_trait]
impl JobControl for Control {
    fn read(&self, stream: JobStream, offset: u64) -> rsi_jobs::Result<JobOutputRead> {
        let output = self
            .stderr
            .lock()
            .map_err(|_| JobsError::InvalidInput("program output lock poisoned".into()))?;
        let Some(output) = output.as_ref().filter(|_| stream == JobStream::Stderr) else {
            return Ok(JobOutputRead {
                full_output: None,
                bytes: vec![],
                oldest_offset: 0,
                next_offset: 0,
                lossy: false,
            });
        };
        let start = usize::try_from(offset.saturating_sub(output.oldest_offset))
            .unwrap_or(usize::MAX)
            .min(output.bytes.len());
        Ok(JobOutputRead {
            full_output: output.full_output.clone(),
            bytes: output.bytes[start..].to_vec(),
            oldest_offset: output.oldest_offset,
            next_offset: output.next_offset,
            lossy: offset < output.oldest_offset,
        })
    }
    fn cancel(&self) {
        self.request.cancel.cancel();
    }
    async fn wait(&self) -> rsi_jobs::Result<JobTerminal> {
        let result = wait_result(self.request.outcome.subscribe()).await;
        Ok(JobTerminal {
            status: if self.request.cancelled_at_settlement.load(Ordering::Acquire) {
                JobStatus::Cancelled
            } else if result.is_ok() {
                JobStatus::Completed
            } else {
                JobStatus::Failed
            },
            exit_code: None,
            signal: None,
            message: result
                .err()
                .map(|message| message.chars().take(1024).collect()),
        })
    }
}
pub(crate) async fn wait_result(
    mut result: watch::Receiver<Option<Result<Value, String>>>,
) -> Result<Value, String> {
    loop {
        if let Some(value) = result.borrow_and_update().clone() {
            return value;
        }
        result
            .changed()
            .await
            .map_err(|_| "program outcome owner closed".to_owned())?;
    }
}
#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
enum Frame {
    Call {
        id: u64,
        method: String,
        arguments: Value,
    },
    Result {
        value: Value,
    },
    Error {
        message: String,
    },
}
async fn execute(
    process: Arc<dyn DuplexProcess>,
    request: &Request,
    stderr: &Mutex<Option<rsi_process::ProcessRead>>,
) -> Result<Value, String> {
    if request.cancel.is_cancelled() {
        return Err("program cancelled".into());
    }
    let process = process
        .spawn(request.spec.clone())
        .map_err(|error| error.to_string())?;
    let result = exchange(process.stdin(), process.stdout(), request).await;
    process.terminate();
    let settlement = process
        .wait_settlement()
        .await
        .map_err(|error| error.to_string());
    if let Ok(read) = process.stderr().read_from(0) {
        *stderr.lock().map_err(|_| "program output lock poisoned")? = Some(read);
    }
    settlement?;
    result
}
async fn exchange(
    input: Arc<dyn DuplexInput>,
    output: Arc<dyn DuplexOutput>,
    request: &Request,
) -> Result<Value, String> {
    write_frame(
        input.as_ref(),
        &json!({"type":"start", "script":request.script, "definitions":request.rpc.definitions(), "maximum_calls":MAXIMUM_PROGRAM_OUTSTANDING_CALLS}),
        &request.cancel,
    )
    .await?;
    let rpc_cancel = request.cancel.child_token();
    let mut calls = FuturesUnordered::new();
    let mut active = BTreeSet::new();
    let mut last_id = 0;
    let mut reading = read_frame(output.clone()).boxed();
    let result = loop {
        tokio::select! {
            biased;
            () = request.cancel.cancelled() => break Err("program cancelled".into()),
            reply = calls.next(), if !active.is_empty() => {
                let Some((id, value)) = reply else { break Err("program RPC owner closed".into()); };
                active.remove(&id);
                let reply = match value { Ok(value) => json!({"type":"reply", "id":id, "value":value}), Err(error) => json!({"type":"reply", "id":id, "error":error}) };
                if let Err(error) = write_frame(input.as_ref(), &reply, &request.cancel).await { break Err(error); }
            }
            frame = &mut reading => {
                let frame = match frame { Ok(frame) => frame, Err(error) => break Err(error) };
                reading = read_frame(output.clone()).boxed();
                match frame {
                    Frame::Call { id, method, arguments } => {
                        if id <= last_id || active.len() >= MAXIMUM_PROGRAM_OUTSTANDING_CALLS || method.len() > 64 { break Err("invalid or excessive program RPC admission".into()); }
                        last_id = id;
                        active.insert(id);
                        let handler = request.rpc.clone();
                        let cancellation = rpc_cancel.clone();
                        calls.push(async move {
                            let result = std::panic::AssertUnwindSafe(handler.call(method, arguments, cancellation)).catch_unwind().await.unwrap_or_else(|_| Err("program RPC handler panicked".into()));
                            (id, result)
                        }.boxed());
                    }
                    Frame::Result { value } => {
                        if !active.is_empty() { break Err("program returned with outstanding RPC calls".into()); }
                        if serde_json::to_vec(&value).map_or(true, |bytes| bytes.len() > MAXIMUM_RESULT) { break Err("program result exceeds 256 KiB".into()); }
                        break Ok(value);
                    }
                    Frame::Error { message } => break Err(message.chars().take(4096).collect()),
                }
            }
        }
    };
    rpc_cancel.cancel();
    while calls.next().await.is_some() {}
    result
}
async fn read_frame(output: Arc<dyn DuplexOutput>) -> Result<Frame, String> {
    let prefix = read_bytes(output.as_ref(), 4).await?;
    let length = usize::try_from(u32::from_be_bytes(
        prefix.try_into().map_err(|_| "invalid program prefix")?,
    ))
    .map_err(|_| "program frame overflow")?;
    if length == 0 || length > MAXIMUM_FRAME {
        return Err("program frame exceeds 1 MiB".into());
    }
    serde_json::from_slice(&read_bytes(output.as_ref(), length).await?)
        .map_err(|error| format!("invalid program frame: {error}"))
}
async fn read_bytes(output: &dyn DuplexOutput, length: usize) -> Result<Vec<u8>, String> {
    let mut bytes = Vec::with_capacity(length.min(rsi_process::MAXIMUM_DUPLEX_CHUNK_BYTES));
    while bytes.len() < length {
        let read = output
            .read((length - bytes.len()).min(rsi_process::MAXIMUM_DUPLEX_CHUNK_BYTES))
            .await
            .map_err(|error| error.to_string())?;
        bytes.extend(read.bytes);
        if read.eof && bytes.len() < length {
            return Err("program ended before its final frame".into());
        }
    }
    Ok(bytes)
}
async fn write_frame(
    input: &dyn DuplexInput,
    value: &Value,
    cancellation: &CancellationToken,
) -> Result<(), String> {
    let body = serde_json::to_vec(value).map_err(|error| error.to_string())?;
    if body.len() > MAXIMUM_FRAME {
        return Err("program frame exceeds 1 MiB".into());
    }
    let prefix = u32::try_from(body.len())
        .map_err(|_| "program frame overflow")?
        .to_be_bytes();
    for bytes in [&prefix[..], &body] {
        let mut offset = 0;
        while offset < bytes.len() {
            let end = (offset + rsi_process::MAXIMUM_DUPLEX_CHUNK_BYTES).min(bytes.len());
            let written = tokio::select! { biased; () = cancellation.cancelled() => return Err("program cancelled".into()), result = input.write(&bytes[offset..end]) => result.map_err(|error| error.to_string())? };
            if written == 0 || written > end - offset {
                return Err("invalid program write progress".into());
            }
            offset += written;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
