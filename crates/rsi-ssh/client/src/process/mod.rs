use rsi_process::{ManagedDuplexProcess, ManagedProcess, ManagedPtyProcess, ProcessError, Result};
use rsi_ssh_protocol::{
    execution::{Preparation, Prepared, Program, StartOptions},
    rpc::{Reply, Request},
};
use rsi_ssh_transport::{Connection, Control, ReceiveStream, Role, SendStream, StreamId};
use std::sync::Arc;
mod execution;
mod files;
pub use execution::execution_provider;
mod output;
pub use files::RemoteFiles;
mod ports;
mod state;
use state::{Owner, State};
#[cfg(test)]
mod tests;

/// Typed client for one already admitted transport; it never establishes SSH trust.
#[derive(Clone, Debug)]
pub struct ProcessConnection {
    transport: Connection,
    _lifetime: Option<Arc<crate::connection::Lifetime>>,
}
/// One-use plan sealed to its issuing connection, including registered byte ports.
#[derive(Debug)]
pub struct RemotePlan {
    connection: ProcessConnection,
    prepared: Prepared,
    input: Option<SendStream>,
    output: Option<ReceiveStream>,
    error: Option<ReceiveStream>,
    consumed: bool,
}
/// One direct remote PTY with an independently observable cancellation ACK.
#[derive(Debug)]
pub struct RemotePtyProcess {
    process: ManagedPtyProcess,
    owner: Arc<Owner>,
}
impl std::ops::Deref for RemotePtyProcess {
    type Target = ManagedPtyProcess;
    fn deref(&self) -> &Self::Target {
        &self.process
    }
}
impl RemotePtyProcess {
    /// Awaits the target's acceptance of termination, separately from process exit.
    /// Losing this waiter retains the one accepted control request.
    pub async fn terminate_acknowledged(&self) -> Result<()> {
        self.owner.0.terminate_acknowledged().await
    }
}
impl RemotePlan {
    /// Native enforcement evidence returned before execution approval.
    pub fn enforcement(&self) -> &rsi_sandbox::EnforcementStamp {
        &self.prepared.enforcement
    }
}
impl Drop for RemotePlan {
    fn drop(&mut self) {
        if !self.consumed {
            self.connection.discard(&self.prepared, false, None);
        }
    }
}
impl ProcessConnection {
    /// Requires the client role and retains the exact connection epoch.
    pub fn new(connection: Connection) -> Result<Self> {
        if connection.role() != Role::Client {
            return Err(invalid());
        }
        Ok(Self {
            transport: connection,
            _lifetime: None,
        })
    }
    pub(crate) fn managed(
        connection: Connection,
        lifetime: Arc<crate::connection::Lifetime>,
    ) -> Self {
        Self {
            transport: connection,
            _lifetime: Some(lifetime),
        }
    }
    /// Returns the transport epoch without granting connection authority.
    pub fn epoch(&self) -> u64 {
        self.transport.epoch()
    }
    /// Reports transport retirement without sending a probe.
    pub fn is_closed(&self) -> bool {
        self.transport.is_closed()
    }
    /// Initializes one fresh helper and checks its independently leased image.
    /// Returns only the requested selectors unavailable in the target namespace.
    pub async fn initialize(
        &self,
        configuration: rsi_ssh_protocol::initialization::Initialization,
        artifact: &[u8; 32],
    ) -> Result<Vec<String>> {
        configuration.validate().map_err(|_| invalid())?;
        let requested = configuration
            .programs
            .keys()
            .cloned()
            .collect::<std::collections::BTreeSet<_>>();
        match self.call(Request::Initialize { configuration }).await? {
            Reply::Ready {
                artifact: actual,
                unavailable,
            } if actual == hex::encode(artifact)
                && unavailable.len() <= requested.len()
                && unavailable
                    .iter()
                    .all(|selector| requested.contains(selector))
                && unavailable.windows(2).all(|pair| pair[0] < pair[1]) =>
            {
                Ok(unavailable)
            }
            _ => Err(self.malformed()),
        }
    }
    /// Resolves only on the target.
    pub async fn canonicalize(&self, path: &str) -> Result<String> {
        rsi_ssh_protocol::execution::validate_path(path).map_err(|_| invalid())?;
        match self
            .call(Request::Canonicalize { path: path.into() })
            .await?
        {
            Reply::Path { path } if rsi_ssh_protocol::execution::validate_path(&path).is_ok() => {
                Ok(path)
            }
            _ => Err(self.malformed()),
        }
    }
    /// Resolves a finite target-owned program/environment selection.
    pub async fn resolve(&self, selector: &str) -> Result<Program> {
        rsi_ssh_protocol::execution::validate_selector(selector).map_err(|_| invalid())?;
        match self
            .call(Request::Resolve {
                selector: selector.into(),
            })
            .await?
        {
            Reply::Program { program } if program.validate().is_ok() => Ok(program),
            _ => Err(self.malformed()),
        }
    }
    /// Resolves a trusted explicit target selection and retains its exact environment.
    pub async fn resolve_configured(
        &self,
        policy: rsi_ssh_protocol::initialization::ProgramPolicy,
    ) -> Result<Program> {
        policy.validate().map_err(|_| invalid())?;
        match self.call(Request::ResolveConfigured { policy }).await? {
            Reply::Program { program } if program.validate().is_ok() => Ok(program),
            _ => Err(self.malformed()),
        }
    }
    /// Prepares exact enforcement and registers output before a start is sent.
    pub async fn prepare(&self, preparation: Preparation) -> Result<RemotePlan> {
        preparation.validate().map_err(|_| invalid())?;
        let client = self.clone();
        retained(async move {
            let prepared = match client
                .call(Request::Prepare {
                    preparation: preparation.clone(),
                })
                .await?
            {
                Reply::Prepared { prepared } if prepared.validate_for(&preparation).is_ok() => {
                    prepared
                }
                _ => return Err(client.malformed()),
            };
            let mut plan = RemotePlan {
                connection: client.clone(),
                prepared,
                input: None,
                output: None,
                error: None,
                consumed: false,
            };
            let ports = (|| {
                plan.input = Some(
                    client
                        .transport
                        .open_sender(StreamId::from_raw(plan.prepared.stdin)?)?,
                );
                plan.output = Some(
                    client
                        .transport
                        .open_receiver(StreamId::from_raw(plan.prepared.stdout)?)?,
                );
                plan.error = plan
                    .prepared
                    .stderr
                    .map(|raw| client.transport.open_receiver(StreamId::from_raw(raw)?))
                    .transpose()?;
                Ok::<_, rsi_ssh_transport::Error>(())
            })();
            if ports.is_err() {
                return Err(client.malformed());
            }
            Ok(plan)
        })
        .await
    }
    /// Consumes a batch plan; uploaded bytes retain the native 4 MiB limit.
    pub async fn spawn(
        &self,
        plan: RemotePlan,
        options: StartOptions,
        stdin: Vec<u8>,
    ) -> Result<ManagedProcess> {
        if !matches!(&options, StartOptions::Batch { stdin_bytes, .. } if *stdin_bytes == stdin.len())
        {
            return Err(invalid());
        }
        let owner = self.start(plan, options, stdin).await?;
        Ok(ManagedProcess::new(owner))
    }
    /// Consumes a duplex plan and exposes native-acknowledged input and lossless output.
    pub async fn spawn_duplex(
        &self,
        plan: RemotePlan,
        options: StartOptions,
    ) -> Result<ManagedDuplexProcess> {
        if !matches!(&options, StartOptions::Duplex { .. }) {
            return Err(invalid());
        }
        Ok(ManagedDuplexProcess::new(
            self.start(plan, options, vec![]).await?,
        ))
    }
    /// Consumes a restricted PTY plan.
    pub async fn spawn_pty(
        &self,
        plan: RemotePlan,
        options: StartOptions,
    ) -> Result<RemotePtyProcess> {
        if !matches!(&options, StartOptions::Pty { .. }) {
            return Err(invalid());
        }
        let owner = self.start(plan, options, vec![]).await?;
        Ok(RemotePtyProcess {
            process: ManagedPtyProcess::new(owner.clone()),
            owner,
        })
    }
    async fn start(
        &self,
        mut plan: RemotePlan,
        options: StartOptions,
        stdin: Vec<u8>,
    ) -> Result<Arc<Owner>> {
        if !self.transport.same_connection(&plan.connection.transport) {
            return Err(invalid());
        }
        options
            .validate(plan.prepared.stderr.is_none())
            .map_err(|_| invalid())?;
        let client = self.clone();
        retained(async move {
            let input = plan.input.take().ok_or_else(invalid)?;
            let request = client.call(Request::Start { handle: plan.prepared.handle, options: options.clone() });
            let rejected = tokio_util::sync::CancellationToken::new();
            let upload = async {
                if matches!(options, StartOptions::Batch { .. }) {
                    tokio::select! { () = rejected.cancelled() => Err(ProcessError::ShuttingDown), result = upload(input, stdin) => result }
                } else { Ok(()) }
            };
            // Poll both futures: helper opens input only after receiving Start.
            let (reply, uploaded) = tokio::join!(async {
                let reply = request.await;
                if !matches!(&reply, Ok(Reply::Started { pid }) if *pid != 0) { rejected.cancel(); }
                reply
            }, upload);
            let reply = match reply {
                Ok(reply) => reply,
                Err(error) => {
                    plan.consumed = true;
                    client.discard(&plan.prepared, true, None);
                    drop(plan);
                    return Err(error);
                }
            };
            if uploaded.is_err() { return Err(client.malformed()); }
            let pid = match reply { Reply::Started { pid } if pid != 0 => pid, _ => return Err(client.malformed()) };
            let output = plan.output.take().ok_or_else(invalid)?;
            let error = plan.error.take();
            let state = State::new(client, plan.prepared.clone(), pid, &options, output, error);
            plan.consumed = true;
            drop(plan);
            Ok(Arc::new(Owner(state)))
        }).await
    }
    async fn call(&self, request: Request) -> Result<Reply> {
        let payload = serde_json::to_vec(&request).map_err(|_| invalid())?;
        let message = self
            .transport
            .call(payload)
            .await
            .map_err(transport_error)?;
        self.reply(message.as_bytes())
    }
    async fn control(&self, control: Control) -> Result<()> {
        let message = self
            .transport
            .control(control)
            .await
            .map_err(transport_error)?;
        match self.reply(message.as_bytes())? {
            Reply::Done => Ok(()),
            _ => Err(self.malformed()),
        }
    }
    fn reply(&self, bytes: &[u8]) -> Result<Reply> {
        match serde_json::from_slice::<Reply>(bytes) {
            Ok(Reply::Failed { failure }) => Err(failure.into()),
            Ok(reply) => Ok(reply),
            Err(_) => Err(self.malformed()),
        }
    }
    fn malformed(&self) -> ProcessError {
        self.transport.close();
        ProcessError::OutcomeUnknown
    }
    async fn cleanup_terminate(&self, handle: u64) -> Result<()> {
        let result = tokio::time::timeout(std::time::Duration::from_secs(30), async {
            let mut backoff = CapacityBackoff::default();
            loop {
                match self.control(Control::Terminate { process: handle }).await {
                    Err(ProcessError::Capacity) => {
                        backoff.wait().await;
                    }
                    result => return result,
                }
            }
        })
        .await;
        result.unwrap_or_else(|_| Err(self.malformed()))
    }
    fn retire_streams(&self, prepared: &Prepared) {
        for id in [Some(prepared.stdin), Some(prepared.stdout), prepared.stderr]
            .into_iter()
            .flatten()
        {
            if let Ok(id) = StreamId::from_raw(id) {
                let _ = self.transport.retire_stream(id);
            }
        }
    }
    fn discard(&self, prepared: &Prepared, started: bool, state: Option<Arc<state::State>>) {
        if !started {
            self.retire_streams(prepared);
        }
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            self.transport.close();
            return;
        };
        let client = self.clone();
        let prepared = prepared.clone();
        runtime.spawn(async move {
            let cleanup = async {
                client.cleanup_terminate(prepared.handle).await?;
                if started {
                    if let Some(state) = state {
                        state.wait_settlement().await?;
                    } else {
                        let mut backoff = CapacityBackoff::default();
                        loop {
                            match client
                                .call(Request::Status {
                                    handle: prepared.handle,
                                })
                                .await
                            {
                                Ok(Reply::Status {
                                    outcome: Some(_),
                                    settlement: Some(Ok(())),
                                }) => break,
                                Ok(Reply::Status {
                                    settlement: Some(Err(_)),
                                    ..
                                }) => return Err(client.malformed()),
                                Ok(Reply::Status { .. }) => {}
                                Err(ProcessError::Capacity) => backoff.wait().await,
                                _ => return Err(client.malformed()),
                            }
                        }
                    }
                    client.retire_streams(&prepared);
                    let mut backoff = CapacityBackoff::default();
                    loop {
                        match client
                            .call(Request::Release {
                                handle: prepared.handle,
                            })
                            .await
                        {
                            Ok(Reply::Done) => break,
                            Err(ProcessError::Capacity) => {
                                backoff.wait().await;
                            }
                            _ => return Err(client.malformed()),
                        }
                    }
                }
                Ok(())
            };
            if !matches!(
                tokio::time::timeout(
                    std::time::Duration::from_millis(rsi_process::MAXIMUM_PROCESS_GRACE_MS + 5000),
                    cleanup
                )
                .await,
                Ok(Ok(()))
            ) {
                client.transport.close();
            }
        });
    }
}
struct CapacityBackoff(std::time::Duration);
impl Default for CapacityBackoff {
    fn default() -> Self {
        Self(std::time::Duration::from_millis(1))
    }
}
impl CapacityBackoff {
    async fn wait(&mut self) {
        tokio::time::sleep(self.0).await;
        self.0 = (self.0 * 2).min(std::time::Duration::from_millis(100));
    }
}
async fn upload(mut stream: SendStream, bytes: Vec<u8>) -> Result<()> {
    let mut chunks = bytes
        .chunks(rsi_ssh_protocol::frame::MAXIMUM_FRAGMENT_BYTES)
        .peekable();
    if chunks.peek().is_none() {
        return stream.send(vec![], true).await.map_err(transport_error);
    }
    while let Some(chunk) = chunks.next() {
        stream
            .send(chunk.to_vec(), chunks.peek().is_none())
            .await
            .map_err(transport_error)?;
    }
    Ok(())
}
fn invalid() -> ProcessError {
    ProcessError::InvalidInput("invalid SSH process operation".into())
}
fn transport_error(error: rsi_ssh_transport::Error) -> ProcessError {
    match error {
        rsi_ssh_transport::Error::OutcomeUnknown => ProcessError::OutcomeUnknown,
        rsi_ssh_transport::Error::Closed => ProcessError::ShuttingDown,
        rsi_ssh_transport::Error::Capacity => ProcessError::Capacity,
        rsi_ssh_transport::Error::Invalid => invalid(),
    }
}
async fn retained<T: Send + 'static>(
    future: impl std::future::Future<Output = Result<T>> + Send + 'static,
) -> Result<T> {
    let (send, receive) = tokio::sync::oneshot::channel();
    tokio::spawn(async move {
        let result = future.await;
        // A queued but never consumed result still owns T and invokes its Drop cleanup.
        let _ = send.send(result);
    });
    receive.await.map_err(|_| ProcessError::OutcomeUnknown)?
}
