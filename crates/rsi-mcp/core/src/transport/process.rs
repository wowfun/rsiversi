//! Private process ownership, distinct from each caller's bounded protocol exchange.
use super::super::error::{McpError, Result};
use rsi_execution::{ExecutionDuplex, ExecutionDuplexExchange, ExecutionLease};
use rsi_process::{DuplexRead, ManagedDuplexProcess};
use std::sync::{Arc, Mutex};

pub(super) enum Process {
    Local(ManagedDuplexProcess),
    Target {
        resource: ExecutionDuplex,
        bootstrap: Mutex<Option<ExecutionLease>>,
        active: Mutex<Option<Arc<ExecutionDuplexExchange>>>,
    },
}
pub(super) struct Exchange<'a> {
    process: &'a Process,
    pub writer: Option<Arc<ExecutionDuplexExchange>>,
}
impl Exchange<'_> {
    pub fn finish(&self) {
        if let Process::Target { active, .. } = self.process {
            active.lock().expect("MCP target exchange").take();
        }
    }
}
impl Process {
    pub fn target(resource: ExecutionDuplex, lease: ExecutionLease) -> Self {
        Self::Target {
            resource,
            bootstrap: Mutex::new(Some(lease)),
            active: Mutex::new(None),
        }
    }
    pub fn seal(&self) {
        if let Self::Target { bootstrap, .. } = self {
            bootstrap.lock().expect("MCP bootstrap").take();
        }
    }
    pub fn exchange(&self, lease: Option<&ExecutionLease>) -> Result<Exchange<'_>> {
        let writer = match self {
            Self::Local(_) => None,
            Self::Target {
                resource,
                bootstrap,
                active,
            } => {
                let bootstrap = bootstrap.lock().expect("MCP bootstrap");
                let lease = lease
                    .or(bootstrap.as_ref())
                    .ok_or(McpError::ProcessUnavailable)?;
                let exchange = Arc::new(resource.exchange(lease).map_err(process_error)?);
                *active.lock().expect("MCP target exchange") = Some(exchange.clone());
                Some(exchange)
            }
        };
        Ok(Exchange {
            process: self,
            writer,
        })
    }
    pub async fn write(
        &self,
        bytes: &[u8],
        exchange: Option<&ExecutionDuplexExchange>,
    ) -> Result<usize> {
        match self {
            Self::Local(process) => process.stdin().write(bytes).await.map_err(process_error),
            Self::Target { resource, .. } => match exchange {
                Some(exchange) => exchange.write(bytes).await.map_err(process_error),
                None => resource.protocol_reply(bytes).await.map_err(process_error),
            },
        }
    }
    pub async fn read(&self, maximum: usize) -> Result<DuplexRead> {
        match self {
            Self::Local(process) => process.stdout().read(maximum).await,
            Self::Target { resource, .. } => resource.read_output(maximum).await,
        }
        .map_err(process_error)
    }
    pub fn terminate(&self) {
        match self {
            Self::Local(process) => process.terminate(),
            Self::Target { resource, .. } => resource.terminate(),
        }
    }
    pub async fn settle(&self) -> Result<()> {
        let result = match self {
            Self::Local(process) => process.wait_settlement().await,
            Self::Target { resource, .. } => resource.wait_settlement().await,
        }
        .map_err(process_error);
        if let Self::Target { active, .. } = self {
            active.lock().expect("MCP target exchange").take();
        }
        result
    }
}
#[expect(
    clippy::needless_pass_by_value,
    reason = "owned map_err adapter discards private backend diagnostics"
)]
pub(crate) fn process_error(error: rsi_process::ProcessError) -> McpError {
    match error {
        rsi_process::ProcessError::OutcomeUnknown => McpError::OutcomeUnknown,
        rsi_process::ProcessError::Capacity => McpError::Capacity,
        _ => McpError::ProcessUnavailable,
    }
}
