use super::output::{Lossless, Tail};
use super::{
    Arc, Prepared, ProcessConnection, ProcessError, ReceiveStream, Reply, Request, Result,
    StartOptions,
};
use rsi_process::ProcessOutcome;
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::sync::watch;
#[derive(Debug)]
pub(super) struct State {
    pub client: ProcessConnection,
    pub prepared: Prepared,
    pub pid: u32,
    pub stdout: Option<Arc<Tail>>,
    pub stderr: Option<Arc<Tail>>,
    pub lossless: Option<Arc<Lossless>>,
    pub outcome: watch::Sender<Option<Result<ProcessOutcome>>>,
    pub pin: std::sync::Mutex<super::execution::PinOwner>,
    settlement: watch::Sender<Option<Result<()>>>,
    terminated: AtomicBool,
    termination_ack: watch::Sender<Option<Result<()>>>,
}
#[derive(Debug)]
pub(super) struct Owner(pub Arc<State>);
impl Drop for Owner {
    fn drop(&mut self) {
        self.0
            .client
            .discard(&self.0.prepared, true, Some(self.0.clone()));
    }
}
impl State {
    pub fn new(
        client: ProcessConnection,
        prepared: Prepared,
        pid: u32,
        options: &StartOptions,
        output: ReceiveStream,
        error: Option<ReceiveStream>,
    ) -> Arc<Self> {
        let (outcome, _) = watch::channel(None);
        let (settlement, _) = watch::channel(None);
        let (stdout, stderr, lossless) = match *options {
            StartOptions::Batch {
                stdout_max_bytes,
                stderr_max_bytes,
                ..
            } => (
                Some(Tail::spawn(output, stdout_max_bytes, client.clone())),
                error.map(|error| Tail::spawn(error, stderr_max_bytes, client.clone())),
                None,
            ),
            StartOptions::Duplex {
                stderr_max_bytes, ..
            } => (
                None,
                error.map(|error| Tail::spawn(error, stderr_max_bytes, client.clone())),
                Some(Lossless::new(output, client.clone())),
            ),
            StartOptions::Pty { .. } => (None, None, Some(Lossless::new(output, client.clone()))),
        };
        let state = Arc::new(Self {
            client,
            prepared,
            pid,
            stdout,
            stderr,
            lossless,
            outcome,
            pin: std::sync::Mutex::default(),
            settlement,
            terminated: AtomicBool::new(false),
            termination_ack: watch::channel(None).0,
        });
        let owner = state.clone();
        tokio::spawn(async move {
            let result = async {
                let mut backoff = super::CapacityBackoff::default();
                loop {
                    match owner
                        .client
                        .call(Request::Status {
                            handle: owner.prepared.handle,
                        })
                        .await
                    {
                        Ok(Reply::Status {
                            outcome: Some(result),
                            settlement: Some(settlement),
                        }) => {
                            if let Ok(outcome) = &result {
                                outcome.validate().map_err(|_| owner.client.malformed())?;
                            }
                            return Ok((
                                result.map(Into::into).map_err(Into::into),
                                settlement.map_err(Into::into),
                            ));
                        }
                        Ok(Reply::Status { .. }) => {}
                        Err(ProcessError::Capacity) => backoff.wait().await,
                        _ => return Err(owner.client.malformed()),
                    }
                }
            }
            .await;
            let (outcome, settlement) =
                result.unwrap_or_else(|error| (Err(error.clone()), Err(error)));
            owner.finish_pin();
            owner.settlement.send_replace(Some(settlement));
            owner.outcome.send_replace(Some(outcome));
        });
        state
    }
    pub fn terminate(&self) {
        if self.terminated.swap(true, Ordering::AcqRel) {
            return;
        }
        let client = self.client.clone();
        let handle = self.prepared.handle;
        let acknowledgement = self.termination_ack.clone();
        tokio::spawn(async move {
            let result = client.cleanup_terminate(handle).await;
            if result.is_err() {
                client.transport.close();
            }
            acknowledgement.send_replace(Some(result));
        });
    }
    pub async fn terminate_acknowledged(&self) -> Result<()> {
        let mut reply = self.termination_ack.subscribe();
        self.terminate();
        loop {
            if let Some(result) = reply.borrow().clone() {
                return result;
            }
            reply
                .changed()
                .await
                .map_err(|_| ProcessError::OutcomeUnknown)?;
        }
    }
    pub async fn settled(&self) -> Result<ProcessOutcome> {
        let mut outcome = self.outcome.subscribe();
        loop {
            if let Some(result) = outcome.borrow().clone() {
                return result;
            }
            outcome
                .changed()
                .await
                .map_err(|_| ProcessError::OutcomeUnknown)?;
        }
    }
    pub async fn wait_settlement(&self) -> Result<()> {
        let mut settlement = self.settlement.subscribe();
        loop {
            if let Some(result) = settlement.borrow().clone() {
                return result;
            }
            settlement
                .changed()
                .await
                .map_err(|_| ProcessError::OutcomeUnknown)?;
        }
    }
    pub async fn captured(&self) -> Result<ProcessOutcome> {
        let outcome = self.settled().await?;
        if let Some(stdout) = &self.stdout {
            stdout.finished().await?;
        }
        if let Some(stderr) = &self.stderr {
            stderr.finished().await?;
        }
        Ok(outcome)
    }
}
