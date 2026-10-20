//! Local Web invocation, asset selection and browser handoff.
use async_trait::async_trait;
use rsi_api_protocol::ApiError;
use rsi_credentials_protocol::SecretValue;
use rsi_meta::{ActivationPlan, ConfigValue, MetaError, PluginFactory, PreparedActivation};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    ffi::OsString,
    path::PathBuf,
    sync::{Arc, Mutex},
    time::Duration,
};

/// Pure local Web help, available without application initialization.
pub const HELP: &str = "Usage: rsi web [--port PORT] [--no-open] [--assets ABSOLUTE_DIRECTORY]\n\
  --port PORT     Loopback port (default 8787; 0 selects a free port)\n\
  --no-open       Print the launch link without opening a browser\n\
  --assets PATH   Override assets/ beside the executable\n\
  --help          Show this Web help without starting the application\n\
  --reset-state  Launcher option; place before application arguments\n\
Equivalent: rsi --profile web [OPTIONS]\n";

/// Explicit OS browser handoff, injectable without opening a real browser in tests.
#[async_trait]
pub trait BrowserOpener: std::fmt::Debug + Send + Sync {
    /// Opens a launch URL; errors must never echo the URL or child output.
    async fn open(&self, url: &str) -> Result<(), String>;
}

/// Explicit debug-launch handoff. Its owner retains the frontend socket and pipes.
#[async_trait]
pub trait WebDevelopmentHandoff: std::fmt::Debug + Send + Sync {
    /// Exact frontend Origin established before bootstrap, stable for this run.
    fn frontend_origin(&self) -> Option<rsi_api_http::LoopbackBrowserOrigin>;
    /// Enables only the existing renderer-only asset publication watcher.
    fn watch_renderers(&self) -> bool;
    /// Reports the owned backend socket and waits for the proxy-ready release.
    async fn ready(&self, address: std::net::SocketAddr) -> Result<(), String>;
    /// Transfers the one-use launch link without putting it in application logs.
    async fn launch(&self, url: &str, open_browser: bool) -> Result<(), String>;
    /// Records an OS interruption so the debug CLI can preserve its exit meaning.
    fn interrupted(&self);
    /// Cancel-safe, repeatable wait for control-owner retirement or pipe closure.
    async fn stopped(&self);
}

/// OS opener with a launcher-supplied, scrubbed environment.
#[derive(Debug)]
pub struct SystemBrowserOpener {
    /// Explicit executable, normally xdg-open or wslview on Linux/WSL.
    pub command: OsString,
    /// Fixed OS launcher arguments preceding the separately passed URL.
    pub arguments: Vec<OsString>,
    /// Only desktop/session discovery variables, never provider credentials.
    pub environment: BTreeMap<OsString, OsString>,
}
#[async_trait]
impl BrowserOpener for SystemBrowserOpener {
    async fn open(&self, url: &str) -> Result<(), String> {
        let mut child = tokio::process::Command::new(&self.command)
            .args(&self.arguments)
            .arg(url)
            .env_clear()
            .envs(&self.environment)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .map_err(|_| "browser opener unavailable".to_owned())?;
        match tokio::time::timeout(Duration::from_secs(5), child.wait()).await {
            Ok(Ok(status)) if status.success() => Ok(()),
            _ => {
                let _ = child.kill().await;
                Err("browser opener failed or timed out".into())
            }
        }
    }
}

/// Explicit application inputs and optional live handoff; no Service identity.
#[derive(Clone, Debug, Default)]
pub struct WebLaunchEnvironment {
    /// Default absolute bundle path captured beside the actual executable.
    pub assets: Option<PathBuf>,
    /// Whether launch occurred through SSH.
    pub ssh: bool,
    /// Optional explicit browser-opening implementation.
    pub opener: Option<Arc<dyn BrowserOpener>>,
    /// Explicit development control, supplied only by the debug CLI entry.
    pub development: Option<Arc<dyn WebDevelopmentHandoff>>,
}

#[derive(Clone, Debug)]
pub(crate) struct Options {
    pub port: u16,
    pub assets: PathBuf,
    pub open: bool,
}

/// Shared immutable invocation for the local Web asset and application factories.
#[derive(Debug)]
pub struct WebLaunch {
    arguments: Vec<OsString>,
    diagnostic: Mutex<Option<crate::RsiError>>,
    pub(crate) environment: WebLaunchEnvironment,
}
impl WebLaunch {
    /// Freezes inputs; validation happens only when this application's leaves prepare.
    pub fn new(arguments: Vec<OsString>, environment: WebLaunchEnvironment) -> Arc<Self> {
        Arc::new(Self {
            arguments,
            environment,
            diagnostic: Mutex::new(None),
        })
    }
    /// Takes the application-owned asset/argument diagnostic after bootstrap failure.
    ///
    /// # Panics
    /// Panics if another thread poisoned the diagnostic mutex.
    pub fn take_diagnostic(&self) -> Option<crate::RsiError> {
        self.diagnostic
            .lock()
            .expect("Web diagnostic poisoned")
            .take()
    }
    fn failure(&self, error: impl std::fmt::Display) -> MetaError {
        let message = error.to_string();
        *self.diagnostic.lock().expect("Web diagnostic poisoned") =
            Some(crate::RsiError::Boot(message.clone()));
        MetaError::InvalidInput(message)
    }
    pub(crate) fn options(&self) -> Result<Options, crate::RsiError> {
        use rsi_application::arguments::{path_value, set_flag, set_option, string_value, utf8};
        if self.arguments.len() > 16
            || self
                .arguments
                .iter()
                .map(|a| a.as_encoded_bytes().len())
                .sum::<usize>()
                > 32 * 1024
        {
            return Err(crate::RsiError::Boot(
                "Web arguments exceed their bound".into(),
            ));
        }
        let mut port = None;
        let mut assets = None;
        let mut no_open = false;
        let mut args = self.arguments.iter().cloned();
        while let Some(arg) = args.next() {
            match utf8(arg)?.as_str() {
                "--port" => {
                    let text = string_value(&mut args, "--port")?;
                    let value = text
                        .parse::<u16>()
                        .ok()
                        .filter(|_| !text.is_empty() && text.bytes().all(|b| b.is_ascii_digit()))
                        .ok_or_else(|| crate::RsiError::Boot("--port requires 0..65535".into()))?;
                    set_option(&mut port, value, "--port")?;
                }
                "--assets" => {
                    set_option(&mut assets, path_value(&mut args, "--assets")?, "--assets")?;
                }
                "--no-open" => set_flag(&mut no_open, "--no-open")?,
                _ => {
                    return Err(crate::RsiError::Boot(
                        "unknown Web option; use rsi web --help".into(),
                    ));
                }
            }
        }
        let assets = assets.or_else(|| self.environment.assets.clone())
            .filter(|p| p.is_absolute() && p.as_os_str().len() <= 16 * 1024)
            .ok_or_else(|| crate::RsiError::Boot("Web requires an absolute --assets directory or a launcher-supplied assets directory".into()))?;
        Ok(Options {
            port: port.unwrap_or(8787),
            assets,
            open: !no_open && !self.environment.ssh,
        })
    }
}

/// Application argument adapter over the ordinary Web asset owner.
#[derive(Debug)]
pub struct LocalWebAssetsFactory(pub Arc<WebLaunch>);
#[async_trait]
impl PluginFactory for LocalWebAssetsFactory {
    fn prepare(&self, desired: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        if !desired.is_null() {
            return Err(MetaError::InvalidInput(
                "local Web assets take application arguments".into(),
            ));
        }
        let options = self.0.options().map_err(|error| self.0.failure(error))?;
        rsi_web_assets::PairedWebAssetsFactory::new(rsi_build_info::family())
            .prepare(&serde_json::json!({"directory": options.assets, "watch": self.0.environment.development.as_ref().is_some_and(|dev| dev.watch_renderers())}))
            .map_err(|error| self.0.failure(error))
    }
    async fn activate(&self, plan: ActivationPlan) -> rsi_meta::Result<()> {
        rsi_web_assets::PairedWebAssetsFactory::new(rsi_build_info::family()).activate(plan).await.map_err(|error| {
            let path = self.0.options().map(|o| o.assets.display().to_string()).unwrap_or_default();
            self.0.failure(format!("Web bundle unavailable at {path}: {error}. Run scripts/rsiversi.sh build web, then scripts/rsiversi.sh web (Windows: scripts/rsiversi.ps1 build web, then scripts/rsiversi.ps1 web)"))
        })
    }
}

pub(crate) struct Ticket {
    value: Mutex<Option<(Vec<u8>, SecretValue)>>,
    expires: rsi_meta::Deadline,
}
impl std::fmt::Debug for Ticket {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WebLaunchTicket").finish_non_exhaustive()
    }
}
impl Ticket {
    pub fn new(
        execution: &rsi_meta::Execution,
        token: SecretValue,
    ) -> rsi_api_protocol::Result<(Arc<Self>, SecretValue)> {
        let mut bytes = [0; 32];
        getrandom::fill(&mut bytes)
            .map_err(|_| ApiError::Backend("launch entropy failed".into()))?;
        let ticket = SecretValue::new(hex::encode(bytes))
            .map_err(|_| ApiError::Backend("launch ticket failed".into()))?;
        Ok((
            Arc::new(Self {
                value: Mutex::new(Some((
                    Sha256::digest(ticket.expose_secret().as_bytes()).to_vec(),
                    token,
                ))),
                expires: execution.deadline_after(Duration::from_mins(10)),
            }),
            ticket,
        ))
    }
    pub fn close(&self) {
        self.value.lock().expect("ticket poisoned").take();
    }
}
impl rsi_api_http::BrowserBootstrap for Ticket {
    fn redeem(&self, ticket: &SecretValue) -> rsi_api_protocol::Result<SecretValue> {
        let mut value = self.value.lock().expect("ticket poisoned");
        if self.expires.has_elapsed() {
            value.take();
            return Err(ApiError::Unauthorized);
        }
        let hash = Sha256::digest(ticket.expose_secret().as_bytes());
        if value
            .as_ref()
            .is_none_or(|(expected, _)| expected.as_slice() != hash.as_slice())
        {
            return Err(ApiError::Unauthorized);
        }
        value
            .take()
            .map(|(_, token)| token)
            .ok_or(ApiError::Unauthorized)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rsi_api_http::BrowserBootstrap;
    use std::path::Path;

    fn assets(name: &str) -> PathBuf {
        Path::new(if cfg!(windows) { "C:/" } else { "/" }).join(name)
    }

    fn launch(args: &[&str], ssh: bool) -> Arc<WebLaunch> {
        WebLaunch::new(
            args.iter().map(OsString::from).collect(),
            WebLaunchEnvironment {
                assets: Some(assets("bundle")),
                ssh,
                opener: None,
                development: None,
            },
        )
    }
    #[test]
    fn local_options_have_one_small_grammar_and_explicit_overrides() {
        let defaults = launch(&[], false).options().unwrap();
        assert_eq!(defaults.port, 8787);
        assert_eq!(defaults.assets, assets("bundle"));
        assert!(defaults.open);
        let custom = assets("custom");
        let overridden = launch(
            &[
                "--port",
                "0",
                "--assets",
                custom.to_str().unwrap(),
                "--no-open",
            ],
            false,
        )
        .options()
        .unwrap();
        assert_eq!(overridden.port, 0);
        assert_eq!(overridden.assets, custom);
        assert!(!overridden.open);
        assert!(!launch(&[], true).options().unwrap().open);
        for args in [
            vec!["--port"],
            vec!["--port", "65536"],
            vec!["--port", "-1"],
            vec!["--port", "+1"],
            vec!["--port", "1", "--port", "2"],
            vec!["--assets", "relative"],
            vec!["--no-open", "--no-open"],
            vec!["--bind", "0.0.0.0:80"],
            vec!["--origin", "http://localhost"],
        ] {
            assert!(launch(&args, false).options().is_err(), "{args:?}");
        }
    }
    #[derive(Debug)]
    #[cfg(any(unix, windows))]
    struct HealthyService;
    #[async_trait]
    #[cfg(any(unix, windows))]
    impl crate::ServingService for HealthyService {
        async fn stopped(&self) -> Result<(), String> {
            std::future::pending().await
        }
        async fn reload(&self) -> Result<(), String> {
            Ok(())
        }
    }
    #[async_trait]
    #[cfg(any(unix, windows))]
    impl rsi_api_http::HttpListener for HealthyService {
        fn address(&self) -> std::net::SocketAddr {
            ([127, 0, 0, 1], 8787).into()
        }
        fn diagnostics(&self) -> rsi_api_http::HttpDiagnostics {
            rsi_api_http::HttpDiagnostics::default()
        }
        async fn stopped(&self) -> rsi_api_protocol::Result<()> {
            std::future::pending().await
        }
    }
    #[derive(Debug, Default)]
    struct DevelopmentHandoff {
        ready: tokio::sync::Notify,
        release: tokio::sync::Notify,
        launched: tokio::sync::Notify,
        stop: tokio_util::sync::CancellationToken,
        link: Mutex<Option<(String, bool)>>,
    }
    #[async_trait]
    impl WebDevelopmentHandoff for DevelopmentHandoff {
        fn frontend_origin(&self) -> Option<rsi_api_http::LoopbackBrowserOrigin> {
            Some(rsi_api_http::LoopbackBrowserOrigin::new("http://127.0.0.1:3001".into()).unwrap())
        }
        fn watch_renderers(&self) -> bool {
            true
        }
        async fn ready(&self, address: std::net::SocketAddr) -> Result<(), String> {
            assert_eq!(address.port(), 8787);
            self.ready.notify_one();
            self.release.notified().await;
            Ok(())
        }
        async fn launch(&self, url: &str, open: bool) -> Result<(), String> {
            *self.link.lock().unwrap() = Some((url.into(), open));
            self.launched.notify_one();
            Ok(())
        }
        fn interrupted(&self) {}
        async fn stopped(&self) {
            self.stop.cancelled().await;
        }
    }
    #[cfg(any(unix, windows))]
    async fn development_handoff_case(release: bool) {
        let handoff = Arc::new(DevelopmentHandoff::default());
        let runner = Arc::new(crate::Runner {
            listener: Arc::new(HealthyService),
            service: Arc::new(HealthyService),
            origin: handoff.frontend_origin().unwrap().as_str().into(),
            launch_link: Mutex::new(Some(SecretValue::new("fixture").unwrap())),
            opener: None,
            development: Some(handoff.clone()),
            open_browser: true,
            started: Mutex::new(false),
            stop: tokio_util::sync::CancellationToken::new(),
            tasks: tokio_util::task::TaskTracker::new(),
            execution: rsi_meta::Execution::native(tokio::runtime::Handle::current()),
        });
        let serving = tokio::spawn(async move { runner.serve().await });
        tokio::time::timeout(Duration::from_secs(5), handoff.ready.notified())
            .await
            .unwrap();
        assert!(handoff.link.lock().unwrap().is_none());
        if release {
            handoff.release.notify_one();
            tokio::time::timeout(Duration::from_secs(5), handoff.launched.notified())
                .await
                .unwrap();
            assert_eq!(
                *handoff.link.lock().unwrap(),
                Some(("http://127.0.0.1:3001#rsi-launch=fixture".into(), true))
            );
        }
        handoff.stop.cancel();
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(5), serving)
                .await
                .unwrap()
                .unwrap(),
            0
        );
        assert_eq!(handoff.link.lock().unwrap().is_some(), release);
    }
    #[tokio::test]
    #[cfg(any(unix, windows))]
    async fn development_handoff_defers_launch_and_preserves_open_preference() {
        development_handoff_case(true).await;
    }
    #[tokio::test]
    #[cfg(any(unix, windows))]
    async fn development_handoff_cancels_during_readiness_without_launching() {
        development_handoff_case(false).await;
    }

    #[derive(Debug, Default)]
    #[cfg(unix)]
    struct FailedOpener(tokio::sync::Notify);
    #[async_trait]
    #[cfg(unix)]
    impl BrowserOpener for FailedOpener {
        async fn open(&self, url: &str) -> Result<(), String> {
            assert_eq!(url, "http://127.0.0.1:8787#rsi-launch=fixture");
            self.0.notify_one();
            Err("fixture failure".into())
        }
    }
    #[tokio::test]
    #[cfg(unix)]
    async fn opener_failure_keeps_serving_and_shutdown_fences_browser_handoff() {
        let opener = Arc::new(FailedOpener::default());
        let runner = Arc::new(crate::Runner {
            listener: Arc::new(HealthyService),
            service: Arc::new(HealthyService),
            origin: "http://127.0.0.1:8787".into(),
            launch_link: Mutex::new(Some(SecretValue::new("fixture").unwrap())),
            opener: Some(opener.clone()),
            development: None,
            open_browser: true,
            started: Mutex::new(false),
            stop: tokio_util::sync::CancellationToken::new(),
            tasks: tokio_util::task::TaskTracker::new(),
            execution: rsi_meta::Execution::native(tokio::runtime::Handle::current()),
        });
        let serving = {
            let runner = runner.clone();
            tokio::spawn(async move { runner.serve().await })
        };
        tokio::time::timeout(Duration::from_secs(5), opener.0.notified())
            .await
            .unwrap();
        assert!(!serving.is_finished());
        runner.stop.cancel();
        assert_eq!(serving.await.unwrap(), 0);
        *runner.launch_link.lock().unwrap() = Some(SecretValue::new("fixture").unwrap());
        assert_eq!(runner.serve().await, 0);
        assert!(runner.launch_link.lock().unwrap().is_some());
    }
    #[tokio::test(start_paused = true)]
    async fn tickets_are_instance_bound_one_use_expiring_and_retired() {
        let token = || SecretValue::new("a".repeat(64)).unwrap();
        let (ticket, secret) = Ticket::new(
            &rsi_meta::Execution::native(tokio::runtime::Handle::current()),
            token(),
        )
        .unwrap();
        let (other, _) = Ticket::new(
            &rsi_meta::Execution::native(tokio::runtime::Handle::current()),
            token(),
        )
        .unwrap();
        assert!(other.redeem(&secret).is_err());
        assert_eq!(
            ticket.redeem(&secret).unwrap().expose_secret(),
            "a".repeat(64)
        );
        assert!(ticket.redeem(&secret).is_err());
        let (ticket, secret) = Ticket::new(
            &rsi_meta::Execution::native(tokio::runtime::Handle::current()),
            token(),
        )
        .unwrap();
        tokio::time::advance(Duration::from_mins(10)).await;
        assert!(ticket.redeem(&secret).is_err());
        let (ticket, secret) = Ticket::new(
            &rsi_meta::Execution::native(tokio::runtime::Handle::current()),
            token(),
        )
        .unwrap();
        ticket.close();
        assert!(ticket.redeem(&secret).is_err());
    }
    #[tokio::test]
    async fn simultaneous_redemptions_have_exactly_one_winner() {
        let (ticket, secret) = Ticket::new(
            &rsi_meta::Execution::native(tokio::runtime::Handle::current()),
            SecretValue::new("a".repeat(64)).unwrap(),
        )
        .unwrap();
        let handles: Vec<_> = (0..8)
            .map(|_| {
                let ticket = ticket.clone();
                let secret = secret.clone();
                std::thread::spawn(move || ticket.redeem(&secret).is_ok())
            })
            .collect();
        assert_eq!(
            handles
                .into_iter()
                .filter_map(|handle| handle.join().ok())
                .filter(|success| *success)
                .count(),
            1
        );
    }
}
