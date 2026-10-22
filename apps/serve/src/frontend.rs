//! Caller-owned frontend processes composed from the ordinary local providers.
use rsi_dev_control::{Frame, MAXIMUM_FRAME_BYTES, Message};
use rsi_meta::{Context, MetaError, PluginFactory};
use rsi_process::{DuplexProcess, DuplexProcessContract, ManagedDuplexProcess};
use rsi_sandbox::{Sandbox, SandboxContract};
use std::{ffi::OsString, io::Read as _, path::PathBuf, sync::Arc, time::Duration};

/// Frozen trusted frontend inputs; Debug excludes arguments and environment.
#[derive(Clone)]
pub struct FrontendRecipe {
    /// Absolute invoking directory.
    pub directory: PathBuf,
    /// Resolved executable followed by literal arguments.
    pub command: Vec<String>,
    /// Complete scrubbed caller environment.
    pub environment: Vec<(OsString, OsString)>,
    /// Ordinary installed package store.
    pub packages: PathBuf,
    /// Requested loopback port; zero selects a free port.
    pub port: u16,
}
impl std::fmt::Debug for FrontendRecipe {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FrontendRecipe").finish_non_exhaustive()
    }
}
fn error(value: impl std::fmt::Display) -> MetaError {
    MetaError::Activation(value.to_string())
}
/// Registers the local contracts used by application-owned process children.
pub fn register_frontend_contracts(
    builder: &mut rsi::StandardAddonBuilder,
) -> rsi_host::Result<()> {
    let scope = rsi::AddonScope::Application;
    builder.register_local_contract_at::<rsi_process::ProcessContract>(scope)?;
    builder.register_local_contract_at::<DuplexProcessContract>(scope)?;
    builder.register_local_contract_at::<rsi_process::PtyProcessContract>(scope)?;
    builder.register_local_contract_at::<SandboxContract>(scope)?;
    Ok(())
}
/// Ordinary provider capabilities retained in the invoking Application scope.
#[derive(Debug)]
pub struct FrontendRuntime {
    recipe: FrontendRecipe,
    process: Arc<dyn DuplexProcess>,
    sandbox: Arc<dyn Sandbox>,
}
impl FrontendRuntime {
    /// Activates isolated ordinary Process and Sandbox children, without another Runtime.
    pub async fn prepare(context: &Context, recipe: FrontendRecipe) -> rsi_meta::Result<Self> {
        if !recipe.directory.is_absolute()
            || recipe.command.is_empty()
            || recipe.command.len() > 128
            || !std::path::Path::new(&recipe.command[0]).is_absolute()
            || recipe
                .command
                .iter()
                .any(|v| v.len() > 16 * 1024 || v.contains('\0'))
            || rsi::scrub_child_environment(recipe.environment.clone()) != recipe.environment
        {
            return Err(error("invalid frozen frontend recipe"));
        }
        let context = context
            .clone()
            .isolate_local_fresh::<rsi_process::ProcessContract>()?
            .0
            .isolate_local_fresh::<DuplexProcessContract>()?
            .0
            .isolate_local_fresh::<rsi_process::PtyProcessContract>()?
            .0
            .isolate_local_fresh::<SandboxContract>()?
            .0;
        for (id, factory) in [
            (
                "rsi.frontend.process",
                Arc::new(rsi_process_local::ProcessLocalFactory) as Arc<dyn PluginFactory>,
            ),
            (
                "rsi.frontend.sandbox",
                Arc::new(rsi_sandbox_local::SandboxLocalFactory::default()),
            ),
        ] {
            let child = context
                .apply(
                    rsi_meta::ResolvedFactory::linked(
                        id,
                        env!("CARGO_PKG_VERSION"),
                        rsi_meta::UpdateMode::RestartRequired,
                        factory,
                    ),
                    serde_json::json!({}),
                )
                .await?;
            if child.snapshot().state != rsi_meta::FiberState::Active {
                return Err(error(format!(
                    "{id} activation failed: {:?}",
                    child.snapshot().state
                )));
            }
        }
        Ok(Self {
            recipe,
            process: context
                .lookup_local::<DuplexProcessContract>()
                .ok_or_else(|| error("missing frontend Process"))?,
            sandbox: context
                .lookup_local::<SandboxContract>()
                .ok_or_else(|| error("missing frontend Sandbox"))?,
        })
    }
    /// Starts the explicit recipe and waits for its actual bound Origin.
    pub async fn start(&self, assets: &rsi_web_control::Assets) -> Result<FrontendSession, String> {
        let (directory, package) = match assets {
            rsi_web_control::Assets::Directory { path } => (path.clone(), None),
            rsi_web_control::Assets::Package { package, export } => {
                let packages = self.recipe.packages.clone();
                let name = package.clone();
                let lease = tokio::task::spawn_blocking(move || {
                    rsi_packages::PackageStore::open(&packages)?.acquire(&name)
                })
                .await
                .map_err(|e| e.to_string())?
                .map_err(|e| e.to_string())?;
                lease
                    .manifest()
                    .admit_interfaces(&crate::supported_protocols())
                    .map_err(|e| e.to_string())?;
                (
                    lease
                        .export(export, rsi_packages::ExportKind::WebAssets)
                        .map_err(|e| e.to_string())?,
                    Some(lease),
                )
            }
        };
        let family = bundle_family(directory).await?;
        let confined = self
            .sandbox
            .confine(rsi_sandbox::ProcessRequest {
                stdio: rsi_sandbox::ProcessStdio::Pipes,
                mode: rsi_sandbox::SandboxMode::DangerFullAccess,
                program: PathBuf::from(&self.recipe.command[0]),
                arguments: self.recipe.command[1..].to_vec(),
                cwd: self.recipe.directory.clone(),
                workspace: self.recipe.directory.clone(),
            })
            .await
            .map_err(|_| "frontend Sandbox admission failed")?;
        let process = self
            .process
            .spawn(rsi_process::DuplexProcessSpec {
                process: confined,
                environment: self.recipe.environment.clone(),
                stdout_buffer_bytes: 64 * 1024,
                stderr_max_bytes: 64 * 1024,
                termination_grace_ms: 1000,
            })
            .await
            .map_err(|_| "frontend process spawn failed")?;
        let mut id = [0; 16];
        getrandom::fill(&mut id).map_err(|_| "frontend identity unavailable")?;
        let mut session = FrontendSession {
            process,
            run_id: hex::encode(id),
            buffered: Vec::new(),
            origin: String::new(),
            _package: package,
        };
        let startup = async {
            session
                .send(Message::StartFrontend {
                    port: self.recipe.port,
                    family,
                })
                .await?;
            let Message::FrontendBound { origin } = session.receive().await? else {
                return Err("frontend did not report bound Origin".into());
            };
            rsi_api_http::LoopbackBrowserOrigin::new(origin)
                .map_err(|_| "frontend Origin invalid")?
                .as_str()
                .clone_into(&mut session.origin);
            Ok(())
        };
        let result = tokio::time::timeout(rsi_dev_control::STARTUP_TIMEOUT, startup)
            .await
            .map_err(|_| "frontend readiness deadline elapsed".to_owned())
            .and_then(|v| v);
        if let Err(error) = result {
            session.close().await;
            return Err(error);
        }
        Ok(session)
    }
}
/// One frontend's process, bounded control stream and immutable bundle lease.
#[derive(Debug)]
pub struct FrontendSession {
    process: ManagedDuplexProcess,
    run_id: String,
    buffered: Vec<u8>,
    origin: String,
    _package: Option<rsi_packages::PackageLease>,
}
impl FrontendSession {
    /// Actual admitted Origin, not a requested or guessed port.
    pub fn origin(&self) -> &str {
        &self.origin
    }
    async fn send(&self, message: Message) -> Result<(), String> {
        let frame = Frame::new(&self.run_id, message).map_err(|_| "invalid frontend command")?;
        let mut bytes =
            serde_json::to_vec(&frame).map_err(|_| "frontend command encoding failed")?;
        bytes.push(b'\n');
        if bytes.len() > MAXIMUM_FRAME_BYTES {
            return Err("frontend command exceeds limit".into());
        }
        let stdin = self.process.stdin();
        let mut remaining = bytes.as_slice();
        while !remaining.is_empty() {
            let written = stdin
                .write(remaining)
                .await
                .map_err(|_| "frontend input closed")?;
            if written == 0 {
                return Err("frontend input closed".into());
            }
            remaining = &remaining[written..];
        }
        Ok(())
    }
    async fn receive(&mut self) -> Result<Message, String> {
        loop {
            if let Some(end) = self.buffered.iter().position(|b| *b == b'\n') {
                let line: Vec<_> = self.buffered.drain(..=end).collect();
                let frame: Frame =
                    serde_json::from_slice(&line).map_err(|_| "invalid frontend control frame")?;
                return frame
                    .into_message(&self.run_id)
                    .map_err(|_| "frontend session differs".into());
            }
            if self.buffered.len() >= MAXIMUM_FRAME_BYTES {
                return Err("frontend frame exceeds limit".into());
            }
            let output = self
                .process
                .stdout()
                .read(MAXIMUM_FRAME_BYTES - self.buffered.len())
                .await
                .map_err(|_| "frontend output failed")?;
            if output.bytes.is_empty() {
                return Err("frontend control closed".into());
            }
            self.buffered.extend(output.bytes);
        }
    }
    /// Waits for exact backend proxy readiness before the UI receives its ticket.
    pub async fn configure(&mut self, address: std::net::SocketAddr) -> Result<(), String> {
        tokio::time::timeout(rsi_dev_control::PROXY_TIMEOUT, async {
            self.send(Message::ConfigureProxy {
                upstream: format!("http://{address}"),
            })
            .await?;
            match self.receive().await? {
                Message::ProxyReady {} => Ok(()),
                _ => Err("frontend did not report proxy readiness".into()),
            }
        })
        .await
        .map_err(|_| "frontend proxy deadline elapsed".to_owned())?
    }
    /// Completes when the frontend exits; exit while the UI runs is a failure.
    pub async fn stopped(&self) {
        let stdout = self.process.stdout();
        tokio::select! {
            _ = self.process.wait() => {},
            () = drain_stdout(stdout.as_ref(), !self.buffered.is_empty()) => {},
        }
    }
    /// Closes control, gives graceful shutdown a bound, and joins descendant settlement.
    pub async fn close(&self) {
        let _ = self.process.stdin().close().await;
        if tokio::time::timeout(Duration::from_secs(5), self.process.wait())
            .await
            .is_err()
        {
            self.process.terminate();
        }
        let _ = self.process.wait_settlement().await;
    }
}

async fn drain_stdout(stdout: &dyn rsi_process::DuplexOutput, buffered: bool) {
    let mut reported = buffered;
    if buffered {
        eprintln!("warning: frontend emitted unexpected stdout after readiness; draining output");
    }
    loop {
        match stdout.read(MAXIMUM_FRAME_BYTES).await {
            Ok(output) if !output.bytes.is_empty() => {
                if !reported {
                    eprintln!(
                        "warning: frontend emitted unexpected stdout after readiness; draining output"
                    );
                    reported = true;
                }
            }
            Ok(_) | Err(_) => return,
        }
    }
}

async fn bundle_family(directory: PathBuf) -> Result<String, String> {
    let family = tokio::task::spawn_blocking(move || {
        let root = rsi_files_native_fs::open_absolute_directory_no_follow(&directory)
            .map_err(|_| "frontend bundle unavailable")?;
        let mut bytes = Vec::new();
        rsi_files_native_fs::open_relative_file_no_follow(
            &root,
            std::path::Path::new("rsi-build.json"),
        )
        .map_err(|_| "frontend bundle receipt unavailable")?
        .take(32 * 1024 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "frontend bundle receipt unreadable")?;
        if bytes.len() > 32 * 1024 {
            return Err("frontend bundle receipt exceeds limit");
        }
        let receipt: serde_json::Value =
            serde_json::from_slice(&bytes).map_err(|_| "frontend bundle receipt invalid")?;
        receipt
            .get("family_sha256")
            .and_then(|v| v.as_str())
            .map(str::to_owned)
            .ok_or("frontend bundle identity missing")
    })
    .await
    .map_err(|_| "frontend receipt worker failed")??;
    Ok(family)
}

#[cfg(test)]
mod output_tests {
    use super::*;
    #[derive(Debug)]
    struct Output(tokio::sync::Mutex<tokio::sync::mpsc::Receiver<Vec<u8>>>);
    #[async_trait::async_trait]
    impl rsi_process::DuplexOutput for Output {
        async fn read(&self, _: usize) -> rsi_process::Result<rsi_process::DuplexRead> {
            let bytes = self.0.lock().await.recv().await.unwrap_or_default();
            Ok(rsi_process::DuplexRead {
                eof: bytes.is_empty(),
                bytes,
            })
        }
    }
    #[tokio::test]
    async fn log_output_does_not_report_exit_before_eof() {
        let (sender, receiver) = tokio::sync::mpsc::channel(2);
        sender.send(b"plugin log\n".to_vec()).await.unwrap();
        let output = Output(tokio::sync::Mutex::new(receiver));
        let drain = drain_stdout(&output, false);
        tokio::pin!(drain);
        assert!(
            tokio::time::timeout(Duration::from_millis(20), &mut drain)
                .await
                .is_err()
        );
        drop(sender);
        tokio::time::timeout(Duration::from_secs(1), drain)
            .await
            .unwrap();
    }
}
