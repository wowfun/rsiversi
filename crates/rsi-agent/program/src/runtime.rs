use crate::ProgramError;
use async_trait::async_trait;
use futures_util::{FutureExt, StreamExt, future::BoxFuture, stream::FuturesUnordered};
use rsi_jobs::{
    JobControl, JobOutputRead, JobProducer, JobRequest, JobStatus, JobStream, JobTerminal,
    JobsError,
};
use rsi_process::{
    DuplexInput, DuplexOutput, DuplexProcess, DuplexProcessSpec, ManagedDuplexProcess,
};
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
    /// Pure descriptions frozen before Jobs admission; a panic refuses admission.
    fn definitions(&self) -> Value;
    /// Executes an admitted request with cooperative cancellation. Construction
    /// or polling panic has an unknown outcome and is never a script reply.
    async fn call(
        &self,
        method: String,
        arguments: Value,
        cancellation: CancellationToken,
    ) -> Result<Value, ProgramError>;
}

type CompletionSender = watch::Sender<Option<Result<Value, ProgramError>>>;

#[derive(Debug)]
pub(crate) struct Request {
    pub spec: Mutex<Option<DuplexProcessSpec<rsi_tools_protocol::ToolProcess>>>,
    pub script: String,
    pub definitions: Value,
    pub rpc: Arc<dyn ProgramRpc>,
    pub start: CancellationToken,
    pub cancel: CancellationToken,
    pub cancelled_at_settlement: AtomicBool,
    pub outcome: watch::Receiver<Option<Result<Value, ProgramError>>>,
    pub completion: Mutex<Option<CompletionSender>>,
}
#[derive(Debug)]
pub(crate) struct Producer {
    pub process: Arc<dyn DuplexProcess>,
    pub tasks: tokio_util::task::TaskTracker,
}
#[async_trait::async_trait]
impl JobProducer for Producer {
    async fn start(&self, request: &JobRequest) -> rsi_jobs::Result<Arc<dyn JobControl>> {
        let request = request
            .downcast_ref::<Arc<Request>>()
            .ok_or_else(|| JobsError::InvalidInput("wrong program producer request".into()))?
            .clone();
        if request.script.len() > MAXIMUM_SCRIPT {
            return Err(JobsError::InvalidInput("script exceeds 64 KiB".into()));
        }
        let control = Arc::new(Control {
            request: request.clone(),
            stderr: Mutex::new(None),
        });
        let completion = request
            .completion
            .lock()
            .map_err(|_| JobsError::InvalidInput("program completion lock poisoned".into()))?
            .take()
            .ok_or_else(|| JobsError::InvalidInput("program was already admitted".into()))?;
        let owner = ExecutionGuard::new(
            Execution {
                rpc_cancel: request.cancel.child_token(),
                request,
                control: control.clone(),
                provider: self.process.clone(),
                spawning: None,
                process: None,
                calls: FuturesUnordered::new(),
                result: None,
                completion: Completion(Some(completion)),
            },
            self.tasks.clone(),
        );
        self.tasks.spawn(owner.run());
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
        let result = wait_result(self.request.outcome.clone()).await;
        Ok(JobTerminal {
            status: if result == Err(ProgramError::OutcomeUnknown) {
                JobStatus::OutcomeUnknown
            } else if self.request.cancelled_at_settlement.load(Ordering::Acquire) {
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
                .map(|message| message.to_string().chars().take(1024).collect()),
        })
    }
}
pub(crate) async fn wait_result(
    mut result: watch::Receiver<Option<Result<Value, ProgramError>>>,
) -> Result<Value, ProgramError> {
    loop {
        if let Some(value) = result.borrow_and_update().clone() {
            return value;
        }
        result
            .changed()
            .await
            .map_err(|_| ProgramError::OutcomeUnknown)?;
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
#[derive(serde::Serialize)]
struct StartFrame<'a> {
    #[serde(rename = "type")]
    kind: &'static str,
    script: &'a str,
    definitions: &'a Value,
    maximum_calls: usize,
}
type Calls = FuturesUnordered<BoxFuture<'static, (u64, Result<Value, ProgramError>)>>;
type Spawn = BoxFuture<'static, Result<ManagedDuplexProcess, ProgramError>>;

struct Completion(Option<CompletionSender>);
impl Completion {
    fn publish(&mut self, result: Result<Value, ProgramError>) {
        if let Some(sender) = self.0.take() {
            sender.send_replace(Some(result));
        }
    }
}
impl Drop for Completion {
    fn drop(&mut self) {
        self.publish(Err(ProgramError::OutcomeUnknown));
    }
}

// Resource slots live outside the caught/abortable driver. A dropped driver
// transfers them, including a move-only pending spawn, without replaying work.
struct Execution {
    request: Arc<Request>,
    control: Arc<Control>,
    provider: Arc<dyn DuplexProcess>,
    spawning: Option<Spawn>,
    process: Option<ManagedDuplexProcess>,
    rpc_cancel: CancellationToken,
    calls: Calls,
    result: Option<Result<Value, ProgramError>>,
    completion: Completion,
}
impl Execution {
    async fn drive(&mut self) -> Result<Value, ProgramError> {
        tokio::select! {
            biased;
            () = self.request.cancel.cancelled() => return Err("program cancelled before start".into()),
            () = self.request.start.cancelled() => {}
        }
        let spec = self
            .request
            .spec
            .lock()
            .map_err(|_| "program plan lock poisoned")?
            .take()
            .ok_or("program plan was already consumed")?;
        let provider = self.provider.clone();
        self.spawning = Some(
            async move {
                contain(async move {
                    rsi_tools_protocol::ToolProcess::spawn_duplex(spec, provider.as_ref())
                        .await
                        .map_err(ProgramError::from)
                })
                .await
            }
            .boxed(),
        );
        self.obtain_process().await?;
        let process = self.process.as_ref().ok_or(ProgramError::OutcomeUnknown)?;
        run_exchange(
            process.stdin(),
            process.stdout(),
            &self.request,
            &mut self.calls,
            &self.rpc_cancel,
        )
        .await
    }

    async fn obtain_process(&mut self) -> Result<(), ProgramError> {
        if let Some(spawning) = self.spawning.as_mut() {
            let result = spawning.await;
            self.spawning.take();
            self.process = Some(result?);
        }
        Ok(())
    }

    async fn settle(&mut self) {
        // Abort can re-enter settlement: termination is idempotent, and Process
        // settlement re-observes retained control. Dropping calls.next() does
        // not drop its queued handlers; the spawn/call futures stay in self.
        if let Err(error) = self.obtain_process().await {
            self.merge_error(error);
        }
        self.rpc_cancel.cancel();
        if let Some(process) = &self.process
            && let Err(error) = contain_sync(|| process.terminate())
        {
            self.merge_error(error);
        }
        let result = self
            .result
            .take()
            .unwrap_or(Err(ProgramError::OutcomeUnknown));
        self.result = Some(drain_calls(&mut self.calls, result).await);
        if let Some(process) = self.process.clone() {
            if let Err(error) =
                contain(async { process.wait_settlement().await.map_err(ProgramError::from) }).await
            {
                self.merge_error(error);
            }
            match contain_sync(|| process.stderr().read_from(0)) {
                Ok(Ok(read)) => {
                    *self
                        .control
                        .stderr
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(read);
                }
                // A panic violates the Process boundary even though ordinary stderr
                // read errors only lose diagnostics; script success is still provisional.
                Err(error) => self.merge_error(error),
                Ok(Err(_)) => {}
            }
        }
        self.request
            .cancelled_at_settlement
            .store(self.request.cancel.is_cancelled(), Ordering::Release);
        self.completion.publish(
            self.result
                .take()
                .unwrap_or(Err(ProgramError::OutcomeUnknown)),
        );
    }

    fn merge_error(&mut self, error: ProgramError) {
        if error == ProgramError::OutcomeUnknown || !matches!(&self.result, Some(Err(_))) {
            self.result = Some(Err(error));
        }
    }
}

// Only a tracked task drives this guard. Dropping the guard precedes dropping
// that task's tracker token, including an abort before its first poll.
struct ExecutionGuard {
    state: Option<Execution>,
    tasks: tokio_util::task::TaskTracker,
    runtime: tokio::runtime::Handle,
}
impl ExecutionGuard {
    fn new(state: Execution, tasks: tokio_util::task::TaskTracker) -> Self {
        Self {
            state: Some(state),
            tasks,
            runtime: tokio::runtime::Handle::current(),
        }
    }
    async fn run(mut self) {
        let state = self.state.as_mut().expect("owned execution");
        state.result = Some(contain(state.drive()).await);
        state.settle().await;
        self.state.take();
    }
}
impl Drop for ExecutionGuard {
    fn drop(&mut self) {
        if let Some(mut state) = self.state.take() {
            state.result = Some(Err(ProgramError::OutcomeUnknown));
            state.request.cancel.cancel();
            state.rpc_cancel.cancel();
            // Acquiring the new tracker token precedes releasing this task's
            // token. The continuation is deliberately not another restart guard.
            // Orderly retirement keeps this runtime alive through the transfer.
            self.tasks
                .spawn_on(async move { state.settle().await }, &self.runtime);
        }
    }
}

pub(crate) async fn contain<T>(
    future: impl std::future::Future<Output = Result<T, ProgramError>>,
) -> Result<T, ProgramError> {
    match std::panic::AssertUnwindSafe(future).catch_unwind().await {
        Ok(result) => result,
        Err(payload) => {
            discard_panic(payload);
            Err(ProgramError::OutcomeUnknown)
        }
    }
}
pub(crate) fn contain_sync<T>(operation: impl FnOnce() -> T) -> Result<T, ProgramError> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(operation)).map_err(|payload| {
        discard_panic(payload);
        ProgramError::OutcomeUnknown
    })
}
fn discard_panic(payload: Box<dyn std::any::Any + Send>) {
    if let Err(payload) = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| drop(payload))) {
        // As in Meta's private containment: a hostile destructor cannot block
        // settlement. Only the payload of that destructor's panic is forgotten.
        if let Err(payload) =
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| drop(payload)))
        {
            std::mem::forget(payload);
        }
    }
}

async fn run_exchange(
    input: Arc<dyn DuplexInput>,
    output: Arc<dyn DuplexOutput>,
    request: &Request,
    calls: &mut Calls,
    rpc_cancel: &CancellationToken,
) -> Result<Value, ProgramError> {
    write_frame(
        input.as_ref(),
        &StartFrame {
            kind: "start",
            script: &request.script,
            definitions: &request.definitions,
            maximum_calls: MAXIMUM_PROGRAM_OUTSTANDING_CALLS,
        },
        &request.cancel,
    )
    .await?;
    let mut active = BTreeSet::new();
    let mut last_id = 0;
    let mut reading = read_frame(output.clone()).boxed();
    loop {
        tokio::select! {
            biased;
            () = request.cancel.cancelled() => break Err("program cancelled".into()),
            reply = calls.next(), if !active.is_empty() => {
                let Some((id, value)) = reply else { break Err("program RPC owner closed".into()); };
                active.remove(&id);
                if value == Err(ProgramError::OutcomeUnknown) { break Err(ProgramError::OutcomeUnknown); }
                let reply = match value { Ok(value) => json!({"type":"reply", "id":id, "value":value}), Err(error) => json!({"type":"reply", "id":id, "error":error.to_string()}) };
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
                            let result = contain(async move { handler.call(method, arguments, cancellation).await }).await;
                            (id, result)
                        }.boxed());
                    }
                    Frame::Result { value } => {
                        if !active.is_empty() { break Err("program returned with outstanding RPC calls".into()); }
                        if serde_json::to_vec(&value).map_or(true, |bytes| bytes.len() > MAXIMUM_RESULT) { break Err("program result exceeds 256 KiB".into()); }
                        break Ok(value);
                    }
                    Frame::Error { mut message } => { message.truncate(message.floor_char_boundary(4096)); break Err(ProgramError::Failed(message)); },
                }
            }
        }
    }
}

async fn drain_calls(
    calls: &mut Calls,
    mut result: Result<Value, ProgramError>,
) -> Result<Value, ProgramError> {
    while let Some((_, reply)) = calls.next().await {
        if reply == Err(ProgramError::OutcomeUnknown) {
            result = Err(ProgramError::OutcomeUnknown);
        }
    }
    result
}

#[cfg(test)]
async fn exchange(
    input: Arc<dyn DuplexInput>,
    output: Arc<dyn DuplexOutput>,
    request: &Request,
) -> Result<Value, ProgramError> {
    let cancel = request.cancel.child_token();
    let mut calls = Calls::new();
    let result = run_exchange(input, output, request, &mut calls, &cancel).await;
    cancel.cancel();
    drain_calls(&mut calls, result).await
}
async fn read_frame(output: Arc<dyn DuplexOutput>) -> Result<Frame, ProgramError> {
    let prefix = read_bytes(output.as_ref(), 4).await?;
    let length = usize::try_from(u32::from_be_bytes(
        prefix.try_into().map_err(|_| "invalid program prefix")?,
    ))
    .map_err(|_| "program frame overflow")?;
    if length == 0 || length > MAXIMUM_FRAME {
        return Err("program frame exceeds 1 MiB".into());
    }
    serde_json::from_slice(&read_bytes(output.as_ref(), length).await?)
        .map_err(|error| ProgramError::Failed(format!("invalid program frame: {error}")))
}
async fn read_bytes(output: &dyn DuplexOutput, length: usize) -> Result<Vec<u8>, ProgramError> {
    let mut bytes = Vec::with_capacity(length.min(rsi_process::MAXIMUM_DUPLEX_CHUNK_BYTES));
    while bytes.len() < length {
        let read = output
            .read((length - bytes.len()).min(rsi_process::MAXIMUM_DUPLEX_CHUNK_BYTES))
            .await
            .map_err(ProgramError::from)?;
        bytes.extend(read.bytes);
        if read.eof && bytes.len() < length {
            return Err("program ended before its final frame".into());
        }
    }
    Ok(bytes)
}
async fn write_frame(
    input: &dyn DuplexInput,
    value: &(impl serde::Serialize + Sync),
    cancellation: &CancellationToken,
) -> Result<(), ProgramError> {
    let body =
        serde_json::to_vec(value).map_err(|error| ProgramError::Failed(error.to_string()))?;
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
            let written = tokio::select! { biased; () = cancellation.cancelled() => return Err("program cancelled".into()), result = input.write(&bytes[offset..end]) => result.map_err(ProgramError::from)? };
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
