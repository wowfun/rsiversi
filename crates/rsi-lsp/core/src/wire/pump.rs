use super::{HEADER_BYTES, MESSAGE_BYTES, length, valid_id};
use crate::{Error, Result};
use futures_util::{FutureExt, future::BoxFuture};
use rsi_process::ManagedDuplexProcess;
use serde_json::{Value, json};
use std::{collections::VecDeque, time::Duration};
use tokio::{
    sync::{mpsc, oneshot, watch},
    time::Instant,
};
use tokio_util::sync::CancellationToken;

type Reply = oneshot::Sender<Result<Value>>;
pub(super) enum Action {
    Begin(Instant),
    End,
    Send {
        method: String,
        params: Value,
        request: bool,
    },
}
struct Command {
    action: Action,
    reply: Reply,
}
#[derive(Debug)]
pub(super) struct Wire {
    process: ManagedDuplexProcess,
    commands: mpsc::Sender<Command>,
    terminal: watch::Receiver<Option<Error>>,
    stop: CancellationToken,
    task: Option<rsi_meta::Task<Result<()>>>,
}
impl Wire {
    pub fn new(
        process: ManagedDuplexProcess,
        configuration: Value,
        execution: &rsi_meta::Execution,
        stop: CancellationToken,
    ) -> Self {
        let (send, commands) = mpsc::channel(1);
        let (terminal, status) = watch::channel(None);
        let mut pump = Pump::new(
            process.clone(),
            configuration,
            commands,
            terminal,
            stop.clone(),
        );
        let task = execution.spawn(async move { pump.run().await });
        Self {
            process,
            commands: send,
            terminal: status,
            stop,
            task: Some(task),
        }
    }
    pub fn failed(&self) -> bool {
        self.terminal.borrow().is_some()
    }
    pub async fn call(&self, action: Action) -> Result<Value> {
        let (reply, receive) = oneshot::channel();
        self.commands
            .send(Command { action, reply })
            .await
            .map_err(|_| self.failure())?;
        receive.await.map_err(|_| self.failure())?
    }
    fn failure(&self) -> Error {
        self.terminal.borrow().unwrap_or(Error::Unavailable)
    }
    pub async fn close(&mut self) -> Result<()> {
        self.stop.cancel();
        if let Some(task) = &mut self.task {
            let result = task.await;
            self.task.take();
            if let Ok(result) = result {
                return result;
            }
            // A failed task did not reach the pump's normal settlement path.
            self.process.terminate();
            self.process
                .wait_settlement()
                .await
                .map_err(|_| Error::Unavailable)?;
            return Err(Error::Unavailable);
        }
        self.process.terminate();
        self.process
            .wait_settlement()
            .await
            .map_err(|_| Error::Unavailable)
    }
}
impl Drop for Wire {
    fn drop(&mut self) {
        self.stop.cancel();
    }
}
struct Budget {
    deadline: Instant,
    bytes: usize,
    requests: usize,
}
struct Rpc {
    id: u64,
    reply: Option<Reply>,
    flushed: bool,
    result: Option<Result<Value>>,
}
enum ReplyBody {
    Configuration(Vec<Option<String>>),
    Progress,
    Unsupported,
}
struct ServerReply {
    id: Value,
    body: ReplyBody,
    deadline: Instant,
}
enum WriteKind {
    Rpc,
    Notify(Reply),
    Server(Instant),
    Grace,
}
struct Writing {
    future: BoxFuture<'static, Result<()>>,
    kind: WriteKind,
}
struct Pump {
    process: ManagedDuplexProcess,
    configuration: Value,
    commands: mpsc::Receiver<Command>,
    terminal: watch::Sender<Option<Error>>,
    stop: CancellationToken,
    buffer: Vec<u8>,
    next: u64,
    budget: Option<Budget>,
    idle_bytes: usize,
    idle_requests: usize,
    active: Option<Rpc>,
    pending: Option<Command>,
    writing: Option<Writing>,
    replies: VecDeque<ServerReply>,
    outstanding: usize,
}
impl Pump {
    fn new(
        process: ManagedDuplexProcess,
        configuration: Value,
        commands: mpsc::Receiver<Command>,
        terminal: watch::Sender<Option<Error>>,
        stop: CancellationToken,
    ) -> Self {
        Self {
            process,
            configuration,
            commands,
            terminal,
            stop,
            buffer: Vec::new(),
            next: 0,
            budget: None,
            idle_bytes: 0,
            idle_requests: 0,
            active: None,
            pending: None,
            writing: None,
            replies: VecDeque::new(),
            outstanding: 0,
        }
    }

    async fn run(&mut self) -> Result<()> {
        let panicked = std::panic::AssertUnwindSafe(self.drive())
            .catch_unwind()
            .await
            .is_err();
        if panicked {
            self.publish_failure(Error::Unavailable);
        }
        self.writing.take();
        self.process.terminate();
        self.process
            .wait_settlement()
            .await
            .map_err(|_| Error::Unavailable)?;
        if panicked {
            Err(Error::Unavailable)
        } else {
            Ok(())
        }
    }

    async fn drive(&mut self) {
        let error = loop {
            match self.step(true).await {
                Ok(()) => {}
                Err(error) => break error,
            }
        };
        self.publish_failure(error);
        self.budget = None;
        // A pending write may have accepted an unknown prefix. No further frame
        // may be appended to it, even a best-effort cancellation notification.
        if self.writing.is_none() {
            // The failed conversation no longer owes these replies. Their old
            // deadlines must not preempt a fresh, bounded retirement attempt.
            self.replies.clear();
            self.outstanding = 0;
            self.grace().await;
        }
    }

    fn publish_failure(&mut self, error: Error) {
        self.terminal.send_replace(Some(error));
        self.commands.close();
        while let Ok(command) = self.commands.try_recv() {
            let _ = command.reply.send(Err(error));
        }
        if let Some(command) = self.pending.take() {
            let _ = command.reply.send(Err(error));
        }
        if let Some(rpc) = &mut self.active
            && let Some(reply) = rpc.reply.take()
        {
            let _ = reply.send(Err(error));
        }
    }
    async fn grace(&mut self) {
        let grace = Duration::from_millis(200);
        if let Some(id) = self.active.as_ref().map(|rpc| rpc.id) {
            if self
                .start_write(
                    &json!({"jsonrpc":"2.0","method":"$/cancelRequest","params":{"id":id}}),
                    WriteKind::Grace,
                )
                .is_err()
            {
                return;
            }
            let _ = tokio::time::timeout(grace, self.finish_write()).await;
            if self.writing.is_some() {
                return;
            }
            let _ = tokio::time::timeout(grace, async {
                while self.active.is_some() {
                    self.step(false).await?;
                }
                Ok::<_, Error>(())
            })
            .await;
        } else {
            let Ok(id) = self.id() else {
                return;
            };
            self.active = Some(Rpc {
                id,
                reply: None,
                flushed: false,
                result: None,
            });
            if self
                .start_write(
                    &json!({"jsonrpc":"2.0","id":id,"method":"shutdown","params":null}),
                    WriteKind::Rpc,
                )
                .is_err()
            {
                return;
            }
            let completed = tokio::time::timeout(grace, async {
                while self.active.is_some() {
                    self.step(false).await?;
                }
                Ok::<_, Error>(())
            })
            .await;
            if !matches!(completed, Ok(Ok(()))) || self.writing.is_some() {
                return;
            }
            if self
                .start_write(
                    &json!({"jsonrpc":"2.0","method":"exit","params":null}),
                    WriteKind::Grace,
                )
                .is_ok()
            {
                let _ = tokio::time::timeout(grace, self.finish_write()).await;
            }
        }
    }
    async fn finish_write(&mut self) -> Result<()> {
        while self.writing.is_some() {
            self.step(false).await?;
        }
        Ok(())
    }
    fn id(&mut self) -> Result<u64> {
        self.next = self
            .next
            .checked_add(1)
            .filter(|id| i32::try_from(*id).is_ok())
            .ok_or(Error::Limit)?;
        Ok(self.next)
    }
    fn start_write(&mut self, value: &Value, kind: WriteKind) -> Result<()> {
        debug_assert!(self.writing.is_none());
        let body = serde_json::to_vec(value).map_err(|_| Error::Protocol)?;
        if body.len() > MESSAGE_BYTES {
            return Err(Error::Limit);
        }
        let bytes = [
            format!("Content-Length: {}\r\n\r\n", body.len()).as_bytes(),
            &body,
        ]
        .concat();
        let input = self.process.stdin();
        let future = Box::pin(async move {
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
        });
        self.writing = Some(Writing { future, kind });
        Ok(())
    }
    fn start_next(&mut self) -> Result<()> {
        if self.writing.is_some() {
            return Ok(());
        }
        if let Some(reply) = self.replies.pop_front() {
            let value = match reply.body {
                ReplyBody::Configuration(sections) => Some(Value::Array(
                    sections
                        .into_iter()
                        .map(|section| {
                            section.map_or_else(
                                || self.configuration.clone(),
                                |section| {
                                    section
                                        .split('.')
                                        .try_fold(&self.configuration, |value, key| value.get(key))
                                        .cloned()
                                        .unwrap_or(Value::Null)
                                },
                            )
                        })
                        .collect(),
                )),
                ReplyBody::Progress => Some(Value::Null),
                ReplyBody::Unsupported => None,
            };
            let value = match value {
                Some(value) => json!({"jsonrpc":"2.0","id":reply.id,"result":value}),
                None => {
                    json!({"jsonrpc":"2.0","id":reply.id,"error":{"code":-32601,"message":"Read-only client method unavailable"}})
                }
            };
            self.start_write(&value, WriteKind::Server(reply.deadline))?;
        } else if let Some(command) = self.pending.take() {
            let Action::Send {
                method,
                params,
                request,
            } = command.action
            else {
                unreachable!()
            };
            if request {
                let id = self.id()?;
                self.active = Some(Rpc {
                    id,
                    reply: Some(command.reply),
                    flushed: false,
                    result: None,
                });
                self.start_write(
                    &json!({"jsonrpc":"2.0","id":id,"method":method,"params":params}),
                    WriteKind::Rpc,
                )?;
            } else {
                self.start_write(
                    &json!({"jsonrpc":"2.0","method":method,"params":params}),
                    WriteKind::Notify(command.reply),
                )?;
            }
        }
        Ok(())
    }
    fn complete_rpc(&mut self) {
        if self
            .active
            .as_ref()
            .is_some_and(|rpc| rpc.flushed && rpc.result.is_some())
        {
            let mut rpc = self.active.take().expect("completed RPC");
            if let Some(reply) = rpc.reply.take() {
                let _ = reply.send(rpc.result.take().expect("received response"));
            }
        }
    }
    fn written(&mut self) {
        match self.writing.take().expect("write completed").kind {
            WriteKind::Rpc => {
                self.active.as_mut().expect("active RPC").flushed = true;
                self.complete_rpc();
            }
            WriteKind::Notify(reply) => {
                let _ = reply.send(Ok(Value::Null));
            }
            WriteKind::Server(_) => self.outstanding -= 1,
            WriteKind::Grace => {}
        }
    }
    fn command(&mut self, command: Command) -> Result<()> {
        match command.action {
            Action::Begin(deadline) => {
                if self.budget.is_some() {
                    return Err(Error::Protocol);
                }
                self.budget = Some(Budget {
                    deadline,
                    bytes: self.buffer.len(),
                    requests: 0,
                });
                let _ = command.reply.send(Ok(Value::Null));
            }
            Action::End => {
                self.budget = None;
                self.idle_bytes = self.buffer.len();
                self.idle_requests = 0;
                let _ = command.reply.send(Ok(Value::Null));
            }
            Action::Send { .. } => self.pending = Some(command),
        }
        Ok(())
    }
    fn append(&mut self, bytes: &[u8]) -> Result<()> {
        if bytes.is_empty() {
            return Err(Error::Unavailable);
        }
        let total = self
            .budget
            .as_mut()
            .map_or(&mut self.idle_bytes, |budget| &mut budget.bytes);
        *total = total.checked_add(bytes.len()).ok_or(Error::Limit)?;
        if *total > 4 * MESSAGE_BYTES {
            return Err(Error::Limit);
        }
        if self.buffer.len() + bytes.len() > MESSAGE_BYTES + HEADER_BYTES + 65536 {
            return Err(Error::Limit);
        }
        self.buffer.extend_from_slice(bytes);
        Ok(())
    }
    fn decode(&mut self) -> Result<bool> {
        for _ in 0..32 {
            let Some(header) = self.buffer.windows(4).position(|w| w == b"\r\n\r\n") else {
                if self.buffer.len() > HEADER_BYTES {
                    return Err(Error::Limit);
                }
                return Ok(false);
            };
            if header > HEADER_BYTES {
                return Err(Error::Limit);
            }
            let end = header + 4 + length(&self.buffer[..header])?;
            if self.buffer.len() < end {
                return Ok(false);
            }
            let value = crate::json::decode(&self.buffer[header + 4..end])?;
            self.buffer.drain(..end);
            self.message(value)?;
        }
        Ok(true)
    }
    fn message(&mut self, mut message: Value) -> Result<()> {
        if message.get("jsonrpc").and_then(Value::as_str) != Some("2.0") || !message.is_object() {
            return Err(Error::Protocol);
        }
        if let Some(method) = message.get("method") {
            let method = method.as_str().ok_or(Error::Protocol)?;
            if message.get("result").is_some() || message.get("error").is_some() {
                return Err(Error::Protocol);
            }
            if let Some(id) = message.get("id") {
                if !valid_id(id) {
                    return Err(Error::Protocol);
                }
                let requests = self
                    .budget
                    .as_mut()
                    .map_or(&mut self.idle_requests, |budget| &mut budget.requests);
                *requests += 1;
                if *requests > 256 {
                    return Err(Error::Limit);
                }
                if self.outstanding == 256 {
                    return Err(Error::Limit);
                }
                let body = match method {
                    "workspace/configuration" => {
                        let items = message
                            .pointer("/params/items")
                            .and_then(Value::as_array)
                            .filter(|items| items.len() <= 16)
                            .ok_or(Error::Protocol)?;
                        ReplyBody::Configuration(
                            items
                                .iter()
                                .map(|item| {
                                    if !item.is_object()
                                        || item.get("scopeUri").is_some_and(|uri| {
                                            uri.as_str().is_none_or(|uri| uri.len() > 4096)
                                        })
                                    {
                                        return Err(Error::Protocol);
                                    }
                                    match item.get("section") {
                                        None => Ok(None),
                                        Some(section) => section
                                            .as_str()
                                            .filter(|s| s.len() <= 128)
                                            .map(|s| Some(s.to_owned()))
                                            .ok_or(Error::Protocol),
                                    }
                                })
                                .collect::<Result<_>>()?,
                        )
                    }
                    "window/workDoneProgress/create" => {
                        if !message.pointer("/params/token").is_some_and(valid_id) {
                            return Err(Error::Protocol);
                        }
                        ReplyBody::Progress
                    }
                    _ => ReplyBody::Unsupported,
                };
                self.replies.push_back(ServerReply {
                    id: id.clone(),
                    body,
                    deadline: Instant::now() + Duration::from_secs(30),
                });
                self.outstanding += 1;
            }
            return Ok(());
        }
        let rpc = self.active.as_mut().ok_or(Error::Protocol)?;
        if message.get("id").and_then(Value::as_u64) != Some(rpc.id)
            || rpc.result.is_some()
            || message.get("result").is_some() == message.get("error").is_some()
        {
            return Err(Error::Protocol);
        }
        rpc.result = Some(if let Some(error) = message.get("error") {
            let code = error
                .get("code")
                .and_then(Value::as_i64)
                .and_then(|code| i32::try_from(code).ok())
                .ok_or(Error::Protocol)?;
            if !error.get("message").is_some_and(Value::is_string) {
                return Err(Error::Protocol);
            }
            Err(Error::Server(code))
        } else {
            Ok(message["result"].take())
        });
        self.complete_rpc();
        Ok(())
    }
    fn deadline(&self) -> Option<Instant> {
        self.budget
            .as_ref()
            .map(|budget| budget.deadline)
            .into_iter()
            .chain(self.replies.front().map(|reply| reply.deadline))
            .chain(self.writing.as_ref().and_then(|write| {
                if let WriteKind::Server(deadline) = write.kind {
                    Some(deadline)
                } else {
                    None
                }
            }))
            .min()
    }
    async fn step(&mut self, running: bool) -> Result<()> {
        if running && self.stop.is_cancelled() {
            return Err(Error::Cancelled);
        }
        if self
            .deadline()
            .is_some_and(|deadline| Instant::now() >= deadline)
        {
            return Err(Error::Deadline);
        }
        let buffered = self.decode()?;
        self.start_next()?;
        let deadline = self.deadline();
        let commands = running
            && self.pending.is_none()
            && self.active.is_none()
            && !self
                .writing
                .as_ref()
                .is_some_and(|write| matches!(write.kind, WriteKind::Notify(_) | WriteKind::Rpc));
        let stdout = self.process.stdout();
        tokio::select! {
            () = self.stop.cancelled(), if running => Err(Error::Cancelled),
            command = self.commands.recv(), if commands => self.command(command.ok_or(Error::Retired)?),
            result = stdout.read(65536) => self.append(&result.map_err(|_| Error::Unavailable)?.bytes),
            result = async { self.writing.as_mut().expect("pending write").future.as_mut().await }, if self.writing.is_some() => { result?; self.written(); Ok(()) },
            () = async { if let Some(deadline) = deadline { tokio::time::sleep_until(deadline).await; } else { std::future::pending::<()>().await; } } => Err(Error::Deadline),
            () = tokio::task::yield_now(), if buffered => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests;
