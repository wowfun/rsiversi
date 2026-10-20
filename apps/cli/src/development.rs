//! Internal debug entry; ordinary Profile and state selection remain authoritative.
use rsi_dev_control::{Frame, Message, STARTUP_TIMEOUT};
use std::{
    ffi::OsString,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU8, Ordering},
    },
};
use tokio::io::BufReader;
use tokio::sync::{Mutex, Notify};
use tokio_util::sync::CancellationToken;

pub(super) async fn run(arguments: Vec<OsString>) -> u8 {
    match execute(arguments).await {
        Ok(code) => code,
        Err(error) => {
            eprintln!("development control: {error}");
            2
        }
    }
}
async fn execute(arguments: Vec<OsString>) -> Result<u8, String> {
    let Some(action) = arguments.first().and_then(|arg| arg.to_str()) else {
        return Err("expected an internal development action".into());
    };
    if action == "tui" {
        let args = &arguments[1..];
        rsi_dev_control::validate_tui_arguments(args).map_err(problem)?;
        return Ok(super::application::run_development(
            super::ApplicationInvocation {
                profile: super::ApplicationProfileId::new("tui").map_err(problem)?,
                arguments: args.to_vec(),
                reset_state: false,
            },
            None,
        )
        .await);
    }
    if arguments.len() != 1 || !matches!(action, "web" | "open-browser") {
        return Err("internal Web control accepts only its child pipe".into());
    }
    let mut input = BufReader::new(tokio::io::stdin());
    let first = tokio::time::timeout(STARTUP_TIMEOUT, rsi_dev_control::read(&mut input))
        .await
        .map_err(|_| "initial control timeout")?
        .map_err(problem)?;
    let run_id = first.run_id().to_owned();
    match (action, first.into_message(&run_id).map_err(problem)?) {
        ("open-browser", Message::OpenBrowser { url }) => {
            let environment = super::application::web_environment().map_err(problem)?;
            if !environment.ssh
                && let Some(opener) = environment.opener
            {
                opener.open(&url).await?;
            }
            Ok(0)
        }
        (
            "web",
            Message::StartBackend {
                port,
                assets,
                watch,
                frontend_origin,
            },
        ) => {
            if watch != frontend_origin.is_some() || (watch && port != 0) {
                return Err("invalid development Web mode".into());
            }
            let frontend_origin = frontend_origin
                .map(rsi_api_http::LoopbackBrowserOrigin::new)
                .transpose()
                .map_err(problem)?;
            let control = Arc::new(Control {
                run_id,
                frontend_origin,
                watch,
                output: Mutex::new(Box::new(tokio::io::stdout())),
                phase: AtomicU8::new(0),
                failed: AtomicBool::new(false),
                interrupted: AtomicBool::new(false),
                released: Notify::new(),
                stop: CancellationToken::new(),
            });
            let reader = tokio::spawn(read_control(input, control.clone()));
            let code = super::application::run_development(
                super::ApplicationInvocation {
                    profile: super::ApplicationProfileId::new("web").map_err(problem)?,
                    arguments: vec![
                        "--port".into(),
                        port.to_string().into(),
                        "--assets".into(),
                        assets.into(),
                    ],
                    reset_state: false,
                },
                Some(control.clone()),
            )
            .await;
            reader.abort();
            let _ = reader.await;
            Ok(if control.failed.load(Ordering::SeqCst) {
                2
            } else if control.interrupted.load(Ordering::SeqCst) {
                130
            } else {
                code
            })
        }
        _ => Err("unexpected internal development action or message".into()),
    }
}
async fn read_control(mut input: impl tokio::io::AsyncBufRead + Unpin, control: Arc<Control>) {
    loop {
        let message = match rsi_dev_control::read(&mut input)
            .await
            .and_then(|frame| frame.into_message(&control.run_id))
        {
            Ok(message) => message,
            Err(error) => {
                if error.kind() != std::io::ErrorKind::UnexpectedEof {
                    control.failed.store(true, Ordering::SeqCst);
                }
                break;
            }
        };
        match message {
            Message::ReleaseLaunch {}
                if control
                    .phase
                    .compare_exchange(1, 2, Ordering::SeqCst, Ordering::SeqCst)
                    .is_ok() =>
            {
                control.released.notify_one();
            }
            _ => {
                control.failed.store(true, Ordering::SeqCst);
                eprintln!("development control: unexpected message order");
                break;
            }
        }
    }
    control.stop.cancel();
}
fn problem(error: impl std::fmt::Display) -> String {
    error.to_string()
}
struct Control {
    run_id: String,
    frontend_origin: Option<rsi_api_http::LoopbackBrowserOrigin>,
    watch: bool,
    output: Mutex<Box<dyn tokio::io::AsyncWrite + Unpin + Send>>,
    phase: AtomicU8,
    failed: AtomicBool,
    interrupted: AtomicBool,
    released: Notify,
    stop: CancellationToken,
}
impl std::fmt::Debug for Control {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DevelopmentControl").finish_non_exhaustive()
    }
}
impl Control {
    async fn send(&self, message: Message) -> Result<(), String> {
        let frame = Frame::new(&self.run_id, message).map_err(problem)?;
        tokio::time::timeout(
            rsi_dev_control::PROXY_TIMEOUT,
            rsi_dev_control::write(&mut *self.output.lock().await, &frame),
        )
        .await
        .map_err(|_| "control write timeout")?
        .map_err(problem)
    }
}
#[async_trait::async_trait]
impl rsi_serve::WebDevelopmentHandoff for Control {
    fn frontend_origin(&self) -> Option<rsi_api_http::LoopbackBrowserOrigin> {
        self.frontend_origin.clone()
    }
    fn watch_renderers(&self) -> bool {
        self.watch
    }
    async fn ready(&self, address: std::net::SocketAddr) -> Result<(), String> {
        if self
            .phase
            .compare_exchange(0, 1, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            return Err("duplicate backend readiness".into());
        }
        self.send(Message::BackendReady {
            address: address.to_string(),
        })
        .await?;
        tokio::select! {
            () = self.stop.cancelled() => Err("development control closed before proxy readiness".into()),
            () = self.released.notified() => Ok(()),
        }
    }
    async fn launch(&self, url: &str, open_browser: bool) -> Result<(), String> {
        if self.stop.is_cancelled()
            || self
                .phase
                .compare_exchange(2, 3, Ordering::SeqCst, Ordering::SeqCst)
                .is_err()
        {
            return Err("launch authority is closed".into());
        }
        self.send(Message::LaunchReady {
            url: url.into(),
            open_browser,
        })
        .await
    }
    async fn stopped(&self) {
        self.stop.cancelled().await;
    }
    fn interrupted(&self) {
        self.interrupted.store(true, Ordering::SeqCst);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rsi_serve::WebDevelopmentHandoff as _;
    fn control() -> Arc<Control> {
        Arc::new(Control {
            run_id: "a".repeat(32),
            frontend_origin: None,
            watch: false,
            output: Mutex::new(Box::new(Vec::<u8>::new())),
            phase: AtomicU8::new(0),
            failed: AtomicBool::new(false),
            interrupted: AtomicBool::new(false),
            released: Notify::new(),
            stop: CancellationToken::new(),
        })
    }
    #[tokio::test(start_paused = true)]
    async fn parent_owns_the_proxy_budget_without_a_competing_backend_timer() {
        let control = control();
        let ready = {
            let control = control.clone();
            tokio::spawn(async move { control.ready(([127, 0, 0, 1], 1234).into()).await })
        };
        tokio::task::yield_now().await;
        tokio::time::advance(std::time::Duration::from_secs(11)).await;
        assert!(!ready.is_finished());
        control.released.notify_one();
        ready.await.unwrap().unwrap();
    }
    #[tokio::test]
    async fn partial_eof_is_a_failed_control_session() {
        let control = control();
        read_control(&b"{\"version\":"[..], control.clone()).await;
        assert!(control.failed.load(Ordering::SeqCst));
        assert!(control.stop.is_cancelled());
    }
    #[tokio::test]
    async fn unknown_action_fails_before_reading_stdin() {
        assert!(execute(vec!["unknown".into()]).await.is_err());
    }
}
