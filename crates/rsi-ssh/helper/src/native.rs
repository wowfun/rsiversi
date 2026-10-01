use crate::{
    HelperError, Result,
    lifecycle::{read_command, runtime_directory},
};
use rsi_execution_local::NativeCapabilities;
use rsi_meta::{FiberHandle, ResolvedFactory, Runtime, UpdateMode};
use rsi_process::{
    DuplexProcessContract, ProcessContract, PtyProcessContract, PtyProcessSpec, PtySize,
};
use rsi_sandbox::{ProcessRequest, ProcessStdio, SandboxContract, SandboxMode};
use rsi_ssh_protocol::{execution::Program, initialization::Initialization};
use std::{collections::BTreeMap, path::PathBuf, sync::Arc, time::Duration};

const TARGET_PATH: &str = "/usr/local/bin:/usr/bin:/bin";
pub(crate) struct NativeRuntime {
    _runtime: Runtime,
    fibers: Vec<FiberHandle>,
    capabilities: NativeCapabilities,
}
impl NativeRuntime {
    pub(crate) async fn new() -> Result<Self> {
        let runtime = Runtime::default();
        let sandbox = ResolvedFactory::linked(
            "sandbox",
            "ssh-helper",
            UpdateMode::Replayable,
            Arc::new(
                rsi_sandbox_local::SandboxLocalFactory::default().require_restricted_backend(),
            ),
        );
        let process = ResolvedFactory::linked(
            "process",
            "ssh-helper",
            UpdateMode::Replayable,
            Arc::new(rsi_process_local::ProcessLocalFactory),
        );
        let sandbox = runtime
            .root()
            .apply(
                sandbox,
                serde_json::json!({"bubblewrap":["/usr/bin/bwrap"],"landlock":[]}),
            )
            .await
            .map_err(|_| HelperError::Unavailable)?;
        let Ok(process) = runtime.root().apply(process, serde_json::json!({})).await else {
            let _ = sandbox.dispose().await;
            return Err(HelperError::Unavailable);
        };
        let capabilities = (|| {
            Ok(NativeCapabilities {
                sandbox: runtime
                    .root()
                    .lookup_local::<SandboxContract>()
                    .ok_or(HelperError::Unavailable)?,
                process: runtime
                    .root()
                    .lookup_local::<ProcessContract>()
                    .ok_or(HelperError::Unavailable)?,
                duplex: runtime
                    .root()
                    .lookup_local::<DuplexProcessContract>()
                    .ok_or(HelperError::Unavailable)?,
                pty: runtime
                    .root()
                    .lookup_local::<PtyProcessContract>()
                    .ok_or(HelperError::Unavailable)?,
                files: Arc::new(
                    rsi_files::LocalFiles::new().map_err(|_| HelperError::Unavailable)?,
                ),
            })
        })();
        let capabilities = match capabilities {
            Ok(capabilities) => capabilities,
            Err(error) => {
                let _ = process.dispose().await;
                let _ = sandbox.dispose().await;
                return Err(error);
            }
        };
        let owner = Self {
            _runtime: runtime,
            fibers: vec![sandbox, process],
            capabilities,
        };
        if let Err(error) = owner.probe_terminal().await {
            let _ = owner.close().await;
            return Err(error);
        }
        Ok(owner)
    }
    pub(crate) fn capabilities(&self) -> NativeCapabilities {
        NativeCapabilities {
            sandbox: self.capabilities.sandbox.clone(),
            process: self.capabilities.process.clone(),
            duplex: self.capabilities.duplex.clone(),
            pty: self.capabilities.pty.clone(),
            files: self.capabilities.files.clone(),
        }
    }
    async fn probe_terminal(&self) -> Result<()> {
        let workspace = tempfile::Builder::new()
            .prefix("rsi-ssh-probe-")
            .tempdir_in(runtime_directory())
            .map_err(|_| HelperError::Unavailable)?;
        let process = self
            .capabilities
            .sandbox
            .confine(ProcessRequest {
                stdio: ProcessStdio::Pty,
                mode: SandboxMode::ReadOnly,
                program: "/usr/bin/true".into(),
                arguments: vec![],
                cwd: workspace.path().to_path_buf(),
                workspace: workspace.path().to_path_buf(),
            })
            .await
            .map_err(|_| HelperError::Unavailable)?;
        let child = self
            .capabilities
            .pty
            .spawn(PtyProcessSpec {
                process,
                environment: vec![],
                size: PtySize {
                    columns: 80,
                    rows: 24,
                },
                termination_grace_ms: 100,
            })
            .await
            .map_err(|_| HelperError::Unavailable)?;
        let result = tokio::time::timeout(Duration::from_secs(3), child.wait()).await;
        if !matches!(
            result,
            Ok(Ok(rsi_process::ProcessOutcome {
                exit_code: Some(0),
                signal: None
            }))
        ) {
            child.terminate();
            let _ = child.wait().await;
            return Err(HelperError::Unavailable);
        }
        Ok(())
    }
    pub(crate) async fn close(self) -> Result<()> {
        let mut clean = true;
        for fiber in self.fibers.into_iter().rev() {
            clean &= fiber.dispose().await.is_clean();
        }
        if clean { Ok(()) } else { Err(HelperError::Io) }
    }
}

pub(crate) async fn programs(
    configuration: Initialization,
) -> Result<(BTreeMap<String, Program>, Vec<String>)> {
    configuration.validate().map_err(|_| HelperError::Invalid)?;
    let account = target_account().await?;
    let mut available = BTreeMap::new();
    let mut unavailable = Vec::new();
    for selector in [
        configuration.apply_patch.then_some("apply_patch"),
        configuration
            .workspace_context
            .then_some("workspace_context"),
        configuration.directory_picker.then_some("directory_picker"),
    ]
    .into_iter()
    .flatten()
    {
        let program = std::env::current_exe()
            .map_err(|_| HelperError::Unavailable)?
            .into_os_string()
            .into_string()
            .map_err(|_| HelperError::Unavailable)?;
        available.insert(
            selector.into(),
            Program {
                program,
                environment: if selector == "directory_picker" {
                    vec![("HOME".into(), account.1.clone())]
                } else {
                    vec![]
                },
            },
        );
    }
    for (selector, policy) in configuration.programs {
        let Some(program) = resolve(&policy.command).await else {
            unavailable.push(selector);
            continue;
        };
        let mut environment = vec![
            ("HOME".into(), account.1.clone()),
            ("USER".into(), account.0.clone()),
            ("LOGNAME".into(), account.0.clone()),
            ("PATH".into(), TARGET_PATH.into()),
        ];
        environment.extend(policy.environment);
        let program = Program {
            program,
            environment,
        };
        program.validate().map_err(|_| HelperError::Invalid)?;
        available.insert(selector, program);
    }
    Ok((available, unavailable))
}
pub(crate) async fn configured_program(
    policy: &rsi_ssh_protocol::initialization::ProgramPolicy,
) -> Result<Option<Program>> {
    policy.validate().map_err(|_| HelperError::Invalid)?;
    let Some(program) = resolve(&policy.command).await else {
        return Ok(None);
    };
    let account = target_account().await?;
    let mut environment = vec![
        ("HOME".into(), account.1),
        ("USER".into(), account.0.clone()),
        ("LOGNAME".into(), account.0),
        ("PATH".into(), TARGET_PATH.into()),
    ];
    environment.extend(policy.environment.clone());
    let program = Program {
        program,
        environment,
    };
    program.validate().map_err(|_| HelperError::Invalid)?;
    Ok(Some(program))
}
async fn target_account() -> Result<(String, String)> {
    static ACCOUNT: tokio::sync::OnceCell<(String, String)> = tokio::sync::OnceCell::const_new();
    ACCOUNT.get_or_try_init(load_target_account).await.cloned()
}
async fn load_target_account() -> Result<(String, String)> {
    let uid = rustix::process::getuid().as_raw();
    let mut command = std::process::Command::new("/usr/bin/getent");
    command.env_clear().args(["passwd", &uid.to_string()]);
    parse_account(&read_command(command).await?, uid)
}
fn parse_account(record: &str, uid: u32) -> Result<(String, String)> {
    let fields = record
        .strip_suffix('\n')
        .unwrap_or(record)
        .split(':')
        .collect::<Vec<_>>();
    if fields.len() != 7
        || fields[2].parse::<u32>().ok() != Some(uid)
        || fields[0].is_empty()
        || fields[0].len() > 256
        || fields[0].chars().any(char::is_control)
    {
        return Err(HelperError::Invalid);
    }
    rsi_ssh_protocol::execution::validate_path(fields[5]).map_err(|_| HelperError::Invalid)?;
    Ok((fields[0].into(), fields[5].into()))
}
async fn resolve(command: &str) -> Option<String> {
    use std::os::unix::fs::PermissionsExt;
    let paths = if command.starts_with('/') {
        vec![PathBuf::from(command)]
    } else {
        TARGET_PATH
            .split(':')
            .map(|directory| PathBuf::from(directory).join(command))
            .collect()
    };
    for path in paths {
        let Ok(path) = tokio::fs::canonicalize(path).await else {
            continue;
        };
        let Ok(metadata) = tokio::fs::metadata(&path).await else {
            continue;
        };
        if metadata.is_file() && metadata.permissions().mode() & 0o111 != 0 {
            return path.into_os_string().into_string().ok();
        }
    }
    None
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn account_records_bind_uid_and_target_home_without_importing_ambient_environment() {
        assert_eq!(
            parse_account("target:x:123:123:Target:/home/target:/bin/bash\n", 123).unwrap(),
            ("target".into(), "/home/target".into())
        );
        for record in [
            "target:x:124:123:Target:/home/target:/bin/bash\n",
            "target:x:123:123:Target:relative:/bin/bash\n",
            "target:x:123:123:Target:/home/target:/bin/bash\nextra:x:123:1::/:/bin/sh\n",
        ] {
            assert!(parse_account(record, 123).is_err());
        }
    }
}
