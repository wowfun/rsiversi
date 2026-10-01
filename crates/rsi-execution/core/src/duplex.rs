//! A server process whose finite protocol exchanges have independent caller authority.
use crate::{ExecutionLease, ExecutionOperation, ExecutionPin, retained};
use rsi_process::{DuplexRead, ManagedDuplexProcess, ProcessError, Result};
use std::{sync::Arc, time::Duration};
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

#[derive(Debug)]
struct ExchangeAdmission {
    permit: std::sync::Mutex<Option<Arc<ExecutionOperation>>>,
    stop: CancellationToken,
}
impl Drop for ExchangeAdmission {
    fn drop(&mut self) {
        self.stop.cancel();
    }
}

/// Private server lifetime and output drain; input requires an admitted exchange.
#[derive(Debug)]
pub struct ExecutionDuplex {
    pub(crate) inner: ManagedDuplexProcess,
    pub(crate) pin: ExecutionPin,
}
impl ExecutionDuplex {
    /// Admits one finite exchange against the exact original provider.
    pub fn exchange(&self, lease: &ExecutionLease) -> Result<ExecutionDuplexExchange> {
        if !Arc::ptr_eq(&self.pin.0.0.provider, &lease.0.provider) {
            return Err(ProcessError::InvalidInput(
                "duplex server belongs to a different execution provider".into(),
            ));
        }
        let admission = Arc::new(ExchangeAdmission {
            permit: std::sync::Mutex::new(Some(Arc::new(ExecutionPin(lease.clone()).admit()?))),
            stop: CancellationToken::new(),
        });
        let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
        let (weak, stop, inner) = (
            Arc::downgrade(&admission),
            admission.stop.clone(),
            self.inner.clone(),
        );
        let pin = ExecutionPin(lease.clone());
        tokio::spawn(async move {
            let _pin = pin;
            tokio::select! { biased;
                () = stop.cancelled() => return,
                () = tokio::time::sleep_until(deadline) => {}
            }
            if let Some(admission) = weak.upgrade() {
                let permit = admission
                    .permit
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .take();
                inner.terminate();
                let _ = inner.wait_settlement().await;
                drop(permit);
            }
        });
        Ok(ExecutionDuplexExchange {
            inner: self.inner.clone(),
            pin: ExecutionPin(lease.clone()),
            admission,
            remaining: Arc::new(Mutex::new(1024 * 1024 + 1)),
            deadline,
        })
    }
    /// Drains accepted output privately; the protocol owner authorizes publication.
    pub async fn read_output(&self, maximum: usize) -> Result<DuplexRead> {
        self.inner.stdout().read(maximum).await
    }
    /// Sends one protocol owner's bounded acknowledgement, never a business request.
    /// This is accepted server maintenance; the protocol owner must bound its reply queue.
    pub async fn protocol_reply(&self, bytes: &[u8]) -> Result<usize> {
        if !(1..=rsi_process::MAXIMUM_DUPLEX_CHUNK_BYTES).contains(&bytes.len()) {
            return Err(ProcessError::Capacity);
        }
        let (inner, bytes) = (self.inner.clone(), bytes.to_vec());
        retained::operation(self.pin.clone(), ExecutionOperation::new(()), async move {
            if let Ok(result) =
                tokio::time::timeout(Duration::from_secs(30), inner.stdin().write(&bytes)).await
            {
                result
            } else {
                inner.terminate();
                let _ = inner.wait_settlement().await;
                Err(ProcessError::OutcomeUnknown)
            }
        })
        .await
    }
    /// Requests cleanup even after the creator's delegation was revoked.
    pub fn terminate(&self) {
        self.inner.terminate();
    }
    /// Waits for native process and pipe settlement independently of publication.
    pub async fn wait_settlement(&self) -> Result<()> {
        self.inner.wait_settlement().await
    }
}
impl Drop for ExecutionDuplex {
    fn drop(&mut self) {
        self.inner.terminate();
    }
}

/// One accepted exchange, bounded to a 1 MiB frame plus newline and 30 seconds.
/// The protocol owner retains it until a verified response or controlled retirement.
#[derive(Debug)]
pub struct ExecutionDuplexExchange {
    inner: ManagedDuplexProcess,
    pin: ExecutionPin,
    admission: Arc<ExchangeAdmission>,
    remaining: Arc<Mutex<usize>>,
    deadline: tokio::time::Instant,
}
impl ExecutionDuplexExchange {
    /// Writes a bounded chunk under the already accepted exchange's authority.
    /// Waiter loss retains the permit and bytes through actual write settlement.
    pub async fn write(&self, bytes: &[u8]) -> Result<usize> {
        if !(1..=rsi_process::MAXIMUM_DUPLEX_CHUNK_BYTES).contains(&bytes.len()) {
            return Err(ProcessError::InvalidInput(
                "invalid duplex exchange chunk".into(),
            ));
        }
        let permit = self
            .admission
            .permit
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_ref()
            .filter(|_| tokio::time::Instant::now() < self.deadline)
            .cloned()
            .ok_or(ProcessError::Capacity)?;
        let (inner, bytes, remaining, deadline) = (
            self.inner.clone(),
            bytes.to_vec(),
            self.remaining.clone(),
            self.deadline,
        );
        retained::operation(
            self.pin.clone(),
            ExecutionOperation::new(permit),
            async move {
                let mut remaining = remaining.lock().await;
                if tokio::time::Instant::now() >= deadline || bytes.len() > *remaining {
                    return Err(ProcessError::Capacity);
                }
                let written = if let Ok(result) =
                    tokio::time::timeout_at(deadline, inner.stdin().write(&bytes)).await
                {
                    result?
                } else {
                    inner.terminate();
                    let _ = inner.wait_settlement().await;
                    return Err(ProcessError::OutcomeUnknown);
                };
                if written > bytes.len() {
                    return Err(ProcessError::OutcomeUnknown);
                }
                *remaining -= written;
                Ok(written)
            },
        )
        .await
    }
}
