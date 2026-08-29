use rsi::{
    OutputMode, RsiError, RunEvent, RunOptions, RunningRsi, SessionSelection, StandardComposition,
    capture_standard_environment, standard_paths,
};
use rsi_agent_session_protocol::{
    MAXIMUM_TURN_TEXT_BYTES, SessionFactBody, SessionId, TurnOutcome,
};
use rsi_ai_protocol::{ContentDelta, LanguageEvent, ModelRef};
use rsi_sandbox::SandboxMode;
use rsi_tools_protocol::ToolContent;
use std::ffi::OsString;
use std::future::Future as _;
use std::io::Read as _;
use std::io::Write;
use std::path::PathBuf;
use std::process::ExitCode;
use std::task::Poll;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

const HELP: &str = "rsi run [TASK | --stdin] [--profile PATH] [--cwd PATH]\n\
    [--resume SESSION | --session-id SESSION]\n\
    [--deployment ID --model ID] [--sandbox MODE] [--output text|jsonl]\n";

#[tokio::main]
async fn main() -> ExitCode {
    ExitCode::from(run_main().await)
}

async fn run_main() -> u8 {
    let command = match Command::parse(std::env::args_os().skip(1)) {
        Ok(Parse::Help) => {
            print!("{HELP}");
            return 0;
        }
        Ok(Parse::Version) => {
            println!("rsi {}", env!("CARGO_PKG_VERSION"));
            return 0;
        }
        Ok(Parse::Run(command)) => command,
        Err(error) => return report_error(&error),
    };
    let task = match command.task().await {
        Ok(task) => task,
        Err(error) => return report_error(&error),
    };
    let paths = match standard_paths() {
        Ok(paths) => paths,
        Err(error) => return report_error(&error),
    };
    let profile_path = command
        .profile
        .clone()
        .unwrap_or_else(|| paths.config().join("profile.toml"));
    let environment = match capture_standard_environment() {
        Ok(environment) => environment,
        Err(error) => return report_error(&error),
    };
    let cancellation = CancellationToken::new();
    let signal_task = match arm_signal(cancellation.clone()).await {
        Ok(task) => task,
        Err(error) => return report_error(&error),
    };
    let running =
        match RunningRsi::boot(StandardComposition::new(paths, environment), &profile_path).await {
            Ok(running) => running,
            Err(error) => {
                signal_task.abort();
                return report_error(&error);
            }
        };

    let options = match command.options(task) {
        Ok(options) => options,
        Err(error) => {
            signal_task.abort();
            let _shutdown = running.shutdown().await;
            return report_error(&error);
        }
    };
    let stdout = std::io::stdout();
    let mut stdout = stdout.lock();
    let mut wrote_text = false;
    let mut text_ends_newline = true;
    let report = running
        .run_turn_observed(options, cancellation, |event| {
            write_live_event(
                &mut stdout,
                command.output,
                event,
                &mut wrote_text,
                &mut text_ends_newline,
            )
        })
        .await;
    signal_task.abort();

    let mut exit = match report {
        Ok(report) => {
            if command.output == OutputMode::Text && wrote_text && !text_ends_newline {
                if let Err(error) = stdout.write_all(b"\n").and_then(|()| stdout.flush()) {
                    report_error(&RsiError::Run(format!("stdout write failed: {error}")))
                } else {
                    if !report.cancellation_requested() {
                        report_terminal_diagnostic(report.outcome());
                    }
                    report.exit_code()
                }
            } else {
                if !report.cancellation_requested() {
                    report_terminal_diagnostic(report.outcome());
                }
                report.exit_code()
            }
        }
        Err(error) => report_error(&error),
    };
    let shutdown = running.shutdown().await;
    if !shutdown.is_clean() {
        eprintln!(
            "shutdown reported {} cleanup failures",
            shutdown.report().total_failures()
        );
        if exit == 0 {
            exit = 1;
        }
    }
    exit
}

async fn arm_signal(cancellation: CancellationToken) -> rsi::Result<JoinHandle<()>> {
    let (armed_tx, armed_rx) = tokio::sync::oneshot::channel();
    let task = tokio::spawn(async move {
        let mut signal = Box::pin(tokio::signal::ctrl_c());
        let initial = std::future::poll_fn(|context| {
            Poll::Ready(match signal.as_mut().poll(context) {
                Poll::Ready(result) => Some(result),
                Poll::Pending => None,
            })
        })
        .await;
        match initial {
            Some(Ok(())) => {
                cancellation.cancel();
                let _ignored = armed_tx.send(Ok(()));
            }
            Some(Err(error)) => {
                let _ignored = armed_tx.send(Err(error));
            }
            None => {
                let _ignored = armed_tx.send(Ok(()));
                if signal.await.is_ok() {
                    cancellation.cancel();
                }
            }
        }
    });
    armed_rx
        .await
        .map_err(|_| RsiError::Boot("SIGINT listener exited before registration".into()))?
        .map_err(|error| RsiError::Boot(format!("failed to register SIGINT listener: {error}")))?;
    Ok(task)
}

fn report_terminal_diagnostic(outcome: &TurnOutcome) {
    match outcome {
        TurnOutcome::Failed { code, message }
        | TurnOutcome::PartialFailed { code, message, .. } => eprintln!("{code}: {message}"),
        TurnOutcome::Interrupted { reason, .. } => eprintln!("interrupted: {reason}"),
        TurnOutcome::Completed | TurnOutcome::Cancelled => {}
    }
}

fn write_live_event(
    stdout: &mut impl Write,
    mode: OutputMode,
    event: &RunEvent,
    wrote_text: &mut bool,
    text_ends_newline: &mut bool,
) -> rsi::Result<()> {
    match mode {
        OutputMode::Jsonl => {
            let line = event
                .json_line()
                .map_err(|error| RsiError::Run(error.to_string()))?;
            stdout
                .write_all(line.as_bytes())
                .and_then(|()| stdout.write_all(b"\n"))
                .and_then(|()| stdout.flush())
                .map_err(|error| RsiError::Run(format!("stdout write failed: {error}")))
        }
        OutputMode::Text => {
            if let RunEvent::Fact { fact, .. } = event {
                match fact.body() {
                    SessionFactBody::ModelEvent {
                        event:
                            LanguageEvent::ContentDelta {
                                delta: ContentDelta::Text(text),
                                ..
                            },
                        ..
                    } => {
                        stdout
                            .write_all(text.as_bytes())
                            .and_then(|()| stdout.flush())
                            .map_err(|error| {
                                RsiError::Run(format!("stdout write failed: {error}"))
                            })?;
                        *wrote_text = true;
                        *text_ends_newline = text.ends_with('\n');
                    }
                    SessionFactBody::ToolResult { result, .. } => {
                        for content in &result.content {
                            if let ToolContent::Image { media } = content {
                                if *wrote_text && !*text_ends_newline {
                                    stdout.write_all(b"\n").map_err(|error| {
                                        RsiError::Run(format!("stdout write failed: {error}"))
                                    })?;
                                }
                                writeln!(stdout, "media:{}", media.id).map_err(|error| {
                                    RsiError::Run(format!("stdout write failed: {error}"))
                                })?;
                                stdout.flush().map_err(|error| {
                                    RsiError::Run(format!("stdout write failed: {error}"))
                                })?;
                                *wrote_text = true;
                                *text_ends_newline = true;
                            }
                        }
                    }
                    SessionFactBody::ImageOutput { media, .. } => {
                        if *wrote_text && !*text_ends_newline {
                            stdout.write_all(b"\n").map_err(|error| {
                                RsiError::Run(format!("stdout write failed: {error}"))
                            })?;
                        }
                        writeln!(stdout, "media:{}", media.id)
                            .and_then(|()| stdout.flush())
                            .map_err(|error| {
                                RsiError::Run(format!("stdout write failed: {error}"))
                            })?;
                        *wrote_text = true;
                        *text_ends_newline = true;
                    }
                    _ => {}
                }
            }
            Ok(())
        }
    }
}

fn report_error(error: &RsiError) -> u8 {
    eprintln!("error: {error}");
    error.exit_code()
}

#[derive(Clone, Debug)]
struct Command {
    positional: Option<String>,
    stdin: bool,
    profile: Option<PathBuf>,
    cwd: Option<PathBuf>,
    resume: Option<SessionId>,
    session_id: Option<SessionId>,
    deployment: Option<String>,
    model: Option<String>,
    sandbox: Option<SandboxMode>,
    output: OutputMode,
}

enum Parse {
    Help,
    Version,
    Run(Command),
}

impl Command {
    fn parse(arguments: impl IntoIterator<Item = OsString>) -> rsi::Result<Parse> {
        let mut arguments = arguments.into_iter();
        let Some(first) = arguments.next() else {
            return Err(usage("missing `run` command"));
        };
        let first = utf8(first)?;
        if matches!(first.as_str(), "-h" | "--help") {
            return Ok(Parse::Help);
        }
        if matches!(first.as_str(), "-V" | "--version") {
            return Ok(Parse::Version);
        }
        if first != "run" {
            return Err(usage("only the `run` command is supported"));
        }

        let mut command = Self {
            positional: None,
            stdin: false,
            profile: None,
            cwd: None,
            resume: None,
            session_id: None,
            deployment: None,
            model: None,
            sandbox: None,
            output: OutputMode::Text,
        };
        let mut literal = false;
        let mut sandbox_set = false;
        let mut output_set = false;
        while let Some(argument) = arguments.next() {
            let argument = utf8(argument)?;
            if !literal && argument == "--" {
                literal = true;
                continue;
            }
            if !literal && argument.starts_with('-') {
                match argument.as_str() {
                    "--stdin" => set_flag(&mut command.stdin, "--stdin")?,
                    "--profile" => set_option(
                        &mut command.profile,
                        path_value(&mut arguments, "--profile")?,
                        "--profile",
                    )?,
                    "--cwd" => set_option(
                        &mut command.cwd,
                        path_value(&mut arguments, "--cwd")?,
                        "--cwd",
                    )?,
                    "--resume" => set_option(
                        &mut command.resume,
                        session_value(&mut arguments, "--resume")?,
                        "--resume",
                    )?,
                    "--session-id" => {
                        set_option(
                            &mut command.session_id,
                            session_value(&mut arguments, "--session-id")?,
                            "--session-id",
                        )?;
                    }
                    "--deployment" => {
                        set_option(
                            &mut command.deployment,
                            string_value(&mut arguments, "--deployment")?,
                            "--deployment",
                        )?;
                    }
                    "--model" => set_option(
                        &mut command.model,
                        string_value(&mut arguments, "--model")?,
                        "--model",
                    )?,
                    "--sandbox" => {
                        if sandbox_set {
                            return Err(usage("duplicate --sandbox"));
                        }
                        sandbox_set = true;
                        command.sandbox = Some(sandbox_value(&mut arguments)?);
                    }
                    "--output" => {
                        if output_set {
                            return Err(usage("duplicate --output"));
                        }
                        output_set = true;
                        command.output = output_value(&mut arguments)?;
                    }
                    "-h" | "--help" => return Ok(Parse::Help),
                    _ => return Err(usage(format!("unknown option `{argument}`"))),
                }
            } else if command.positional.replace(argument).is_some() {
                return Err(usage("exactly one task positional is allowed"));
            }
        }
        command.validate()?;
        Ok(Parse::Run(command))
    }

    fn validate(&self) -> rsi::Result<()> {
        if self.stdin == self.positional.is_some() {
            return Err(usage("provide exactly one task positional or --stdin"));
        }
        if self.resume.is_some() && self.session_id.is_some() {
            return Err(usage("--resume and --session-id are mutually exclusive"));
        }
        if self.deployment.is_some() != self.model.is_some() {
            return Err(usage("--deployment and --model must be supplied together"));
        }
        if let (Some(deployment), Some(model)) = (&self.deployment, &self.model) {
            ModelRef::new(deployment, model).map_err(|error| usage(error.to_string()))?;
        }
        Ok(())
    }

    async fn task(&self) -> rsi::Result<String> {
        if let Some(task) = &self.positional {
            return Ok(task.clone());
        }
        let input = tokio::task::spawn_blocking(|| {
            let mut input = Vec::new();
            std::io::stdin()
                .take(u64::try_from(MAXIMUM_TURN_TEXT_BYTES).unwrap_or(u64::MAX) + 1)
                .read_to_end(&mut input)
                .map(|_| input)
        })
        .await
        .map_err(|error| RsiError::Boot(format!("stdin worker failed: {error}")))?
        .map_err(|error| RsiError::Boot(format!("stdin read failed: {error}")))?;
        if input.len() > MAXIMUM_TURN_TEXT_BYTES {
            return Err(usage("stdin task exceeds the Agent text bound"));
        }
        String::from_utf8(input).map_err(|_| usage("stdin task is not UTF-8"))
    }

    fn options(&self, task: String) -> rsi::Result<RunOptions> {
        let session = match &self.resume {
            Some(session_id) => SessionSelection::Resume {
                session_id: session_id.clone(),
                cwd: self.cwd.clone(),
            },
            None => SessionSelection::Fresh {
                cwd: match &self.cwd {
                    Some(cwd) => cwd.clone(),
                    None => std::env::current_dir().map_err(|error| {
                        RsiError::Boot(format!("current directory is unavailable: {error}"))
                    })?,
                },
                session_id: self.session_id.clone(),
            },
        };
        let model = self
            .deployment
            .as_ref()
            .zip(self.model.as_ref())
            .map(|(deployment, model)| {
                ModelRef::new(deployment, model).map_err(|error| usage(error.to_string()))
            })
            .transpose()?;
        Ok(RunOptions {
            task,
            session,
            model,
            sandbox: self.sandbox,
            output: self.output,
        })
    }
}

fn sandbox_value(arguments: &mut impl Iterator<Item = OsString>) -> rsi::Result<SandboxMode> {
    match string_value(arguments, "--sandbox")?.as_str() {
        "read-only" => Ok(SandboxMode::ReadOnly),
        "workspace-write" => Ok(SandboxMode::WorkspaceWrite),
        "danger-full-access" => Ok(SandboxMode::DangerFullAccess),
        _ => Err(usage("invalid --sandbox mode")),
    }
}

fn output_value(arguments: &mut impl Iterator<Item = OsString>) -> rsi::Result<OutputMode> {
    match string_value(arguments, "--output")?.as_str() {
        "text" => Ok(OutputMode::Text),
        "jsonl" => Ok(OutputMode::Jsonl),
        _ => Err(usage("invalid --output mode")),
    }
}

fn set_flag(value: &mut bool, name: &str) -> rsi::Result<()> {
    if *value {
        return Err(usage(format!("duplicate {name}")));
    }
    *value = true;
    Ok(())
}

fn set_option<T>(slot: &mut Option<T>, value: T, name: &str) -> rsi::Result<()> {
    if slot.is_some() {
        return Err(usage(format!("duplicate {name}")));
    }
    *slot = Some(value);
    Ok(())
}

fn path_value(
    arguments: &mut impl Iterator<Item = OsString>,
    option: &str,
) -> rsi::Result<PathBuf> {
    arguments
        .next()
        .map(PathBuf::from)
        .ok_or_else(|| usage(format!("{option} requires a value")))
}

fn session_value(
    arguments: &mut impl Iterator<Item = OsString>,
    option: &str,
) -> rsi::Result<SessionId> {
    let value = string_value(arguments, option)?;
    SessionId::new(value).map_err(|error| usage(error.to_string()))
}

fn string_value(
    arguments: &mut impl Iterator<Item = OsString>,
    option: &str,
) -> rsi::Result<String> {
    let value = arguments
        .next()
        .ok_or_else(|| usage(format!("{option} requires a value")))?;
    utf8(value)
}

fn utf8(value: OsString) -> rsi::Result<String> {
    value
        .into_string()
        .map_err(|_| usage("CLI arguments must be UTF-8"))
}

fn usage(message: impl Into<String>) -> RsiError {
    RsiError::Boot(format!("{}\n{HELP}", message.into()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(arguments: &[&str]) -> rsi::Result<Parse> {
        Command::parse(arguments.iter().map(OsString::from))
    }

    #[test]
    fn enforces_input_model_and_session_exclusivity() {
        assert!(parse(&["run"]).is_err());
        assert!(parse(&["run", "task", "--stdin"]).is_err());
        assert!(parse(&["run", "task", "--deployment", "one"]).is_err());
        assert!(
            parse(&[
                "run",
                "task",
                "--resume",
                "session-one",
                "--session-id",
                "session-two"
            ])
            .is_err()
        );
        assert!(
            parse(&[
                "run",
                "task",
                "--deployment",
                "contains space",
                "--model",
                "model"
            ])
            .is_err()
        );
        assert!(parse(&["run", "task", "--output", "text", "--output", "jsonl"]).is_err());
    }

    #[test]
    fn leading_slash_is_plain_task_and_dash_task_uses_separator() {
        let Parse::Run(command) = parse(&["run", "/status"]).unwrap() else {
            panic!("run")
        };
        assert_eq!(command.positional.as_deref(), Some("/status"));
        let Parse::Run(command) = parse(&["run", "--", "--literal"]).unwrap() else {
            panic!("run")
        };
        assert_eq!(command.positional.as_deref(), Some("--literal"));
    }
}
