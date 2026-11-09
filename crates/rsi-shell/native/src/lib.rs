//! Native Bash and PowerShell capability plugins.

#![deny(unsafe_code)]
#![warn(missing_docs)]
#![allow(clippy::missing_errors_doc)]

use async_trait::async_trait;
use rsi_jobs::JobsContract;
use rsi_meta::{ActivationPlan, ConfigValue, MetaError, PluginFactory, PreparedActivation};
use rsi_process::ProcessContract;
use rsi_tools_protocol::{ToolError, ToolRegistrarContract};
use serde_json::Value;
use std::ffi::OsString;
use std::path::{Path, PathBuf};

/// Maximum UTF-8 bytes in one shell command argument.
pub const MAXIMUM_SHELL_COMMAND_BYTES: usize = 96 * 1024;
/// Default foreground shell timeout.
pub const DEFAULT_SHELL_TIMEOUT_MS: u64 = 120_000;
/// Maximum foreground shell timeout.
pub const MAXIMUM_SHELL_TIMEOUT_MS: u64 = 570_000;
/// Retained raw bytes per shell output stream.
pub const SHELL_STREAM_CAPTURE_BYTES: usize = 64_000;
/// TERM-to-KILL grace for a shell process group.
pub const SHELL_TERMINATION_GRACE_MS: u64 = 3_000;
/// Outer timeout for the model-facing shell tool.
pub const SHELL_TOOL_TIMEOUT_MS: u64 = 600_000;

mod environment;
pub use environment::scrub_child_environment;
mod dialect;
pub use dialect::ShellKind;

/// Ordinary factory for one shell kind's stable Job producer.
#[derive(Clone, Debug)]
pub struct ShellJobProducerFactory {
    kind: ShellKind,
}
impl ShellJobProducerFactory {
    /// Selects the shell kind whose stable producer is registered.
    pub const fn new(kind: ShellKind) -> Self {
        Self { kind }
    }
}

/// Ordinary factory for the model-facing shell tool.
#[derive(Clone, Debug)]
pub struct ShellToolFactory {
    kind: ShellKind,
    program: PathBuf,
    environment: std::sync::Arc<[(OsString, OsString)]>,
}

impl ShellToolFactory {
    /// Freezes one shell dialect, invocation path and noninteractive environment.
    pub fn new(
        kind: ShellKind,
        program: impl Into<PathBuf>,
        environment: Vec<(OsString, OsString)>,
    ) -> rsi_tools_protocol::Result<Self> {
        let program = program.into();
        if !program.is_absolute() {
            return Err(ToolError::InvalidInput(
                "shell program must be absolute".into(),
            ));
        }
        let environment = environment::prepare(environment)?;
        Ok(Self {
            kind,
            program,
            environment: environment.into(),
        })
    }
    /// The command dialect and model-facing tool identity.
    pub const fn kind(&self) -> ShellKind {
        self.kind
    }
    /// The explicit executable invocation path.
    pub fn executable(&self) -> &Path {
        &self.program
    }
    /// The frozen, scrubbed environment with noninteractive defaults.
    pub fn environment(&self) -> &[(OsString, OsString)] {
        &self.environment
    }

    fn retained_bytes(&self) -> rsi_meta::Result<usize> {
        self.environment
            .iter()
            .try_fold(self.program.as_os_str().len(), |bytes, (name, value)| {
                bytes.checked_add(name.len())?.checked_add(value.len())
            })
            .ok_or_else(|| MetaError::InvalidInput("shell tool retained bytes overflow".into()))
    }
}

#[async_trait]
impl PluginFactory for ShellJobProducerFactory {
    fn prepare(&self, desired: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        if !desired.is_null() {
            return Err(MetaError::InvalidInput(
                "shell Job producer configuration must be null".into(),
            ));
        }
        Ok(PreparedActivation::with_state(Value::Null, (), 0)
            .requiring_local::<JobsContract>()
            .requiring_local::<ProcessContract>())
    }

    async fn activate(&self, plan: ActivationPlan) -> rsi_meta::Result<()> {
        activate_producer(plan, self.kind)
    }
}

#[async_trait]
impl PluginFactory for ShellToolFactory {
    fn prepare(&self, desired: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        if !desired.is_null() {
            return Err(MetaError::InvalidInput(
                "shell tool configuration must be null".into(),
            ));
        }
        Ok(
            PreparedActivation::with_state(Value::Null, (), self.retained_bytes()?)
                .requiring_local::<ToolRegistrarContract>()
                .requiring_local::<JobsContract>()
                .requiring_local::<ProcessContract>(),
        )
    }

    async fn activate(&self, plan: ActivationPlan) -> rsi_meta::Result<()> {
        activate_tool(plan, self)
    }
}

#[cfg(any(target_os = "linux", windows))]
fn activate_producer(plan: ActivationPlan, kind: ShellKind) -> rsi_meta::Result<()> {
    execution::activate_producer(plan, kind)
}

#[cfg(not(any(target_os = "linux", windows)))]
fn activate_producer(mut plan: ActivationPlan, kind: ShellKind) -> rsi_meta::Result<()> {
    let (): () = plan.take_state()?;
    let _ = kind;
    Err(MetaError::Activation(
        "shell Jobs require native Linux or Windows process settlement".into(),
    ))
}

#[cfg(any(target_os = "linux", windows))]
fn activate_tool(plan: ActivationPlan, factory: &ShellToolFactory) -> rsi_meta::Result<()> {
    execution::activate_tool(plan, factory)
}

#[cfg(not(any(target_os = "linux", windows)))]
fn activate_tool(mut plan: ActivationPlan, factory: &ShellToolFactory) -> rsi_meta::Result<()> {
    let (): () = plan.take_state()?;
    let _ = factory;
    Err(MetaError::Activation(
        "the shell tool requires native Linux or Windows process settlement".into(),
    ))
}

#[cfg(any(target_os = "linux", windows))]
mod execution;
