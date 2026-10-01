//! A retained terminal resource with separately admitted operation views.
use crate::{ExecutionLease, ExecutionPin, retained};
use rsi_process::{ManagedPtyProcess, ProcessError, ProcessOutcome, PtyRead, PtySize, Result};
use std::sync::Arc;

/// An accepted terminal's lifetime and private output projection, without standing input authority.
#[derive(Debug)]
pub struct ExecutionPty {
    pub(crate) inner: ManagedPtyProcess,
    pub(crate) pin: ExecutionPin,
}
impl ExecutionPty {
    /// Admits a current caller view only through the exact original provider tuple.
    pub fn view(&self, lease: &ExecutionLease) -> Result<ExecutionPtyView> {
        if !Arc::ptr_eq(&self.pin.0.0.provider, &lease.0.provider) {
            return Err(ProcessError::InvalidInput(
                "terminal belongs to a different execution provider".into(),
            ));
        }
        let _permit = ExecutionPin(lease.clone()).publication()?;
        Ok(ExecutionPtyView {
            inner: self.inner.clone(),
            pin: ExecutionPin(lease.clone()),
        })
    }
    /// Drains accepted output into the owner's private bounded projection.
    /// The product must separately authorize every publication to an external caller.
    pub async fn read_output(&self) -> Result<PtyRead> {
        self.inner.read().await
    }
    /// Requests termination independently of caller revocation.
    pub fn terminate(&self) {
        self.inner.terminate();
    }
    /// Confirms native settlement; resource drop alone does not establish it.
    pub async fn wait(&self) -> Result<ProcessOutcome> {
        self.inner.wait().await
    }
}

/// A caller-bound operation view. Dropping it releases only this view, never the terminal.
#[derive(Debug)]
pub struct ExecutionPtyView {
    inner: ManagedPtyProcess,
    pin: ExecutionPin,
}
impl ExecutionPtyView {
    /// Writes one bounded input with current delegation retained through settlement.
    pub async fn write(&self, bytes: &[u8]) -> Result<usize> {
        if !(1..=rsi_process::MAXIMUM_PTY_IO_BYTES).contains(&bytes.len()) {
            return Err(ProcessError::InvalidInput(
                "terminal input exceeds its bound".into(),
            ));
        }
        let permit = self.pin.admit()?;
        let (inner, bytes) = (self.inner.clone(), bytes.to_vec());
        retained::operation(self.pin.clone(), permit, async move {
            inner.write(&bytes).await
        })
        .await
    }
    /// Publishes a resize only after its backend acknowledges the operation.
    pub async fn resize(&self, size: PtySize) -> Result<()> {
        size.validate()?;
        let permit = self.pin.admit()?;
        let inner = self.inner.clone();
        retained::operation(
            self.pin.clone(),
            permit,
            async move { inner.resize(size).await },
        )
        .await
    }
}
impl Drop for ExecutionPty {
    fn drop(&mut self) {
        self.inner.terminate();
    }
}
