use rsi::{
    AgentPresetManager, AgentPresetSource, AgentPresetTrust, OutputMode, RsiError, RunEvent,
    RunOptions, RunningRsi, SessionSelection, StandardCodingTools, StandardComposition,
    capture_standard_environment, maybe_run_apply_patch_helper, scrub_child_environment,
    standard_agent_preset_root, standard_paths,
};
use rsi_agent_presets::{AgentPresetHealth, AgentPresetId, AgentPresetRow, PresetError};
use rsi_agent_session_protocol::{
    MAXIMUM_TURN_TEXT_BYTES, SessionFactBody, SessionId, TurnOutcome,
};
use rsi_ai_protocol::{ContentDelta, LanguageEvent, ModelRef};
use rsi_sandbox::SandboxMode;
use rsi_tools_protocol::ToolContent;
use serde::Serialize;
use std::ffi::OsString;
use std::future::Future as _;
use std::io::Read as _;
use std::io::Write;
use std::path::PathBuf;
use std::process::ExitCode;
use std::task::Poll;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

const HELP: &str = "Usage:\n\
  rsi run [TASK | --stdin] [--profile PATH] [--cwd PATH]\n\
      [--resume SESSION | --session-id SESSION] [--agent-preset ID]\n\
      [--deployment ID --model ID] [--sandbox MODE] [--output text|jsonl]\n\
  rsi agent-preset <COMMAND> [--output text|json]\n\n\
Commands:\n\
  run             Run one Agent turn\n\
  agent-preset    Inspect and manage local Agent presets\n";
const AGENT_PRESET_HELP: &str = "Usage:\n\
  rsi agent-preset list [--output text|json]\n\
  rsi agent-preset show ID [--output text|json]\n\
  rsi agent-preset path ID [--output text|json]\n\
  rsi agent-preset copy --from SOURCE --id ID [--name NAME] [--output text|json]\n\
  rsi agent-preset delete ID [--output text|json]\n\
  rsi agent-preset default <get|set ID|clear> [--output text|json]\n\n\
Commands:\n\
  list       List the fresh precedence-resolved roster\n\
  show       Show one row and its bounded composition when healthy\n\
  path       Print the winning local preset directory\n\
  copy       Copy a discovered preset into the user root\n\
  delete     Delete a winning user-root preset\n\
  default    Get, set, or clear the user default\n";
const AGENT_PRESET_DEFAULT_HELP: &str = "Usage:\n\
  rsi agent-preset default get [--output text|json]\n\
  rsi agent-preset default set ID [--output text|json]\n\
  rsi agent-preset default clear [--output text|json]\n\n\
Commands:\n\
  get      Print the effective default\n\
  set      Store one syntactically valid preset id\n\
  clear    Re-inherit the deployment default\n";
const BOOT_FAILURE_EXIT_CODE: u8 = 2;

fn main() -> ExitCode {
    if let Some(exit) = maybe_run_apply_patch_helper(std::env::args_os().skip(1)) {
        return ExitCode::from(exit);
    }
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("failed to construct Tokio runtime: {error}");
            return ExitCode::from(BOOT_FAILURE_EXIT_CODE);
        }
    };
    ExitCode::from(runtime.block_on(run_main()))
}

#[allow(clippy::too_many_lines)] // Boot, live output, terminal diagnostics, and shutdown form one CLI ownership transaction.
async fn run_main() -> u8 {
    let command = match Command::parse(std::env::args_os().skip(1)) {
        Ok(Parse::Help(help)) => {
            print!("{help}");
            return 0;
        }
        Ok(Parse::Version) => {
            println!("rsi {}", env!("CARGO_PKG_VERSION"));
            return 0;
        }
        Ok(Parse::Run(command)) => command,
        Ok(Parse::AgentPreset(command)) => return run_agent_preset(command).await,
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
    let coding_tools = match standard_coding_tools() {
        Ok(coding_tools) => coding_tools,
        Err(error) => return report_error(&error),
    };
    let options = match command.options(task) {
        Ok(options) => options,
        Err(error) => return report_error(&error),
    };
    let system_root = match standard_agent_preset_root(&paths) {
        Ok(root) => root,
        Err(error) => return report_error(&RsiError::Boot(error.to_string())),
    };
    let presets =
        match AgentPresetManager::open_standard(paths.clone(), system_root, coding_tools.is_some())
            .await
        {
            Ok(manager) => manager,
            Err(error) => return report_error(&error),
        };
    let cancellation = CancellationToken::new();
    let signal_task = match arm_signal(cancellation.clone()).await {
        Ok(task) => task,
        Err(error) => {
            let exit = report_error(&error);
            return shutdown_agent_preset_manager(presets, exit, 2, "run bootstrap").await;
        }
    };
    let running = match RunningRsi::boot(
        StandardComposition::new(paths, environment, coding_tools)
            .with_agent_presets(presets.catalog().clone()),
        &profile_path,
    )
    .await
    {
        Ok(running) => running,
        Err(error) => {
            signal_task.abort();
            let exit = report_error(&error);
            return shutdown_agent_preset_manager(presets, exit, 2, "run bootstrap").await;
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
    shutdown_agent_preset_manager(presets, exit, 1, "run").await
}

async fn run_agent_preset(command: AgentPresetCommand) -> u8 {
    let paths = match standard_paths() {
        Ok(paths) => paths,
        Err(error) => return report_error(&error),
    };
    let system_root = match standard_agent_preset_root(&paths) {
        Ok(root) => root,
        Err(error) => return report_error(&RsiError::Boot(error.to_string())),
    };
    let manager = match AgentPresetManager::open_standard(
        paths,
        system_root,
        cfg!(target_os = "linux"),
    )
    .await
    {
        Ok(manager) => manager,
        Err(error) => return report_error(&error),
    };
    let result = execute_agent_preset(&manager, command).await;
    let exit = match result {
        Ok(()) => 0,
        Err(error) => report_error(&error),
    };
    shutdown_agent_preset_manager(manager, exit, 2, "management").await
}

async fn shutdown_agent_preset_manager(
    manager: AgentPresetManager,
    mut exit: u8,
    clean_failure_exit: u8,
    operation: &str,
) -> u8 {
    let shutdown = manager.shutdown().await;
    if !shutdown.is_clean() {
        eprintln!(
            "Agent-preset {operation} shutdown reported {} cleanup failures",
            shutdown.report().total_failures()
        );
        if exit == 0 {
            exit = clean_failure_exit;
        }
    }
    exit
}

async fn execute_agent_preset(
    manager: &AgentPresetManager,
    command: AgentPresetCommand,
) -> rsi::Result<()> {
    match command.operation {
        AgentPresetOperation::List => list_agent_presets(manager, command.output).await,
        AgentPresetOperation::Show(id) => show_agent_preset(manager, command.output, id).await,
        AgentPresetOperation::Path(id) => path_agent_preset(manager, command.output, &id),
        AgentPresetOperation::Copy {
            source,
            target,
            name,
        } => {
            manager
                .catalog()
                .copy(&source, target.clone(), name)
                .await
                .map_err(preset_management_error)?;
            write_action(command.output, "copied", &target)
        }
        AgentPresetOperation::Delete(id) => {
            manager
                .catalog()
                .delete(&id)
                .await
                .map_err(preset_management_error)?;
            write_action(command.output, "deleted", &id)
        }
        AgentPresetOperation::DefaultGet => {
            let id = manager
                .catalog()
                .default_id()
                .await
                .map_err(preset_management_error)?;
            write_default(command.output, "get", &id)
        }
        AgentPresetOperation::DefaultSet(id) => {
            manager
                .catalog()
                .set_default(&id)
                .await
                .map_err(preset_management_error)?;
            write_default(command.output, "set", &id)
        }
        AgentPresetOperation::DefaultClear => {
            manager
                .catalog()
                .clear_default()
                .await
                .map_err(preset_management_error)?;
            let id = manager
                .catalog()
                .default_id()
                .await
                .map_err(preset_management_error)?;
            write_default(command.output, "clear", &id)
        }
    }
}

async fn list_agent_presets(
    manager: &AgentPresetManager,
    output: ManagementOutput,
) -> rsi::Result<()> {
    let roster = manager
        .catalog()
        .roster()
        .await
        .map_err(preset_management_error)?;
    let presets = roster
        .presets
        .into_iter()
        .map(PresetOutput::from)
        .collect::<Vec<_>>();
    match output {
        ManagementOutput::Json => write_json(&ListOutput {
            version: 1,
            kind: "agent_preset_list",
            authorable: roster.authorable,
            presets,
        }),
        ManagementOutput::Text => write_roster_text(&presets),
    }
}

async fn show_agent_preset(
    manager: &AgentPresetManager,
    output: ManagementOutput,
    id: AgentPresetId,
) -> rsi::Result<()> {
    let roster = manager
        .catalog()
        .roster()
        .await
        .map_err(preset_management_error)?;
    let available = roster
        .presets
        .iter()
        .map(|row| row.id.as_str().to_owned())
        .collect::<Vec<_>>();
    let row = roster
        .presets
        .into_iter()
        .find(|row| row.id == id)
        .ok_or_else(|| {
            preset_management_error(PresetError::PresetNotFound {
                id: id.as_str().to_owned(),
                available,
            })
        })?;
    let composition = if row.health == AgentPresetHealth::Healthy {
        Some(
            manager
                .catalog()
                .document(&id)
                .map_err(preset_management_error)?
                .content,
        )
    } else {
        None
    };
    let preset = PresetOutput::from(row);
    match output {
        ManagementOutput::Json => write_json(&ShowOutput {
            version: 1,
            kind: "agent_preset",
            preset,
            composition,
        }),
        ManagementOutput::Text => write_show_text(&preset, composition.as_deref()),
    }
}

fn path_agent_preset(
    manager: &AgentPresetManager,
    output: ManagementOutput,
    id: &AgentPresetId,
) -> rsi::Result<()> {
    let path = manager
        .catalog()
        .location(id)
        .map_err(preset_management_error)?;
    let path = path
        .to_str()
        .ok_or_else(|| RsiError::Boot("Agent preset path cannot be represented as UTF-8".into()))?;
    match output {
        ManagementOutput::Json => write_json(&PathOutput {
            version: 1,
            kind: "agent_preset_path",
            id: id.as_str(),
            path,
        }),
        ManagementOutput::Text => write_text_line(path),
    }
}

#[derive(Debug, Serialize)]
struct PresetOutput {
    id: String,
    metadata: MetadataOutput,
    source: &'static str,
    trust: &'static str,
    status: &'static str,
    reason: Option<String>,
    default: bool,
}

#[derive(Debug, Serialize)]
struct MetadataOutput {
    name: Option<String>,
    description: Option<String>,
}

impl From<AgentPresetRow> for PresetOutput {
    fn from(row: AgentPresetRow) -> Self {
        let (status, reason) = match row.health {
            AgentPresetHealth::Healthy => ("healthy", None),
            AgentPresetHealth::Broken { reason } => ("broken", Some(reason)),
        };
        Self {
            id: row.id.as_str().to_owned(),
            metadata: MetadataOutput {
                name: row.name,
                description: row.description,
            },
            source: source_name(row.source),
            trust: trust_name(row.trust),
            status,
            reason,
            default: row.is_default,
        }
    }
}

#[derive(Debug, Serialize)]
struct ListOutput {
    version: u8,
    #[serde(rename = "type")]
    kind: &'static str,
    authorable: bool,
    presets: Vec<PresetOutput>,
}

#[derive(Debug, Serialize)]
struct ShowOutput {
    version: u8,
    #[serde(rename = "type")]
    kind: &'static str,
    preset: PresetOutput,
    composition: Option<String>,
}

#[derive(Debug, Serialize)]
struct PathOutput<'a> {
    version: u8,
    #[serde(rename = "type")]
    kind: &'static str,
    id: &'a str,
    path: &'a str,
}

#[derive(Debug, Serialize)]
struct ActionOutput<'a> {
    version: u8,
    #[serde(rename = "type")]
    kind: &'static str,
    action: &'a str,
    id: &'a str,
}

fn source_name(source: AgentPresetSource) -> &'static str {
    match source {
        AgentPresetSource::System => "system",
        AgentPresetSource::Configured => "configured",
        AgentPresetSource::User => "user",
    }
}

fn trust_name(trust: AgentPresetTrust) -> &'static str {
    match trust {
        AgentPresetTrust::System => "system",
        AgentPresetTrust::User => "user",
    }
}

fn write_roster_text(presets: &[PresetOutput]) -> rsi::Result<()> {
    let stdout = std::io::stdout();
    let mut stdout = stdout.lock();
    writeln!(stdout, "DEFAULT\tID\tSOURCE\tTRUST\tHEALTH\tNAME").map_err(output_error)?;
    for preset in presets {
        let health = preset.reason.as_ref().map_or_else(
            || preset.status.to_owned(),
            |reason| format!("broken: {reason}"),
        );
        writeln!(
            stdout,
            "{}\t{}\t{}\t{}\t{}\t{}",
            if preset.default { "*" } else { "" },
            preset.id,
            preset.source,
            preset.trust,
            health,
            preset.metadata.name.as_deref().unwrap_or("")
        )
        .map_err(output_error)?;
    }
    stdout.flush().map_err(output_error)
}

fn write_show_text(preset: &PresetOutput, composition: Option<&str>) -> rsi::Result<()> {
    let stdout = std::io::stdout();
    let mut stdout = stdout.lock();
    writeln!(stdout, "id: {}", preset.id).map_err(output_error)?;
    writeln!(stdout, "source: {}", preset.source).map_err(output_error)?;
    writeln!(stdout, "trust: {}", preset.trust).map_err(output_error)?;
    writeln!(
        stdout,
        "default: {}",
        if preset.default { "yes" } else { "no" }
    )
    .map_err(output_error)?;
    writeln!(stdout, "health: {}", preset.status).map_err(output_error)?;
    if let Some(reason) = &preset.reason {
        writeln!(stdout, "reason: {reason}").map_err(output_error)?;
    }
    if let Some(name) = &preset.metadata.name {
        writeln!(stdout, "name: {name}").map_err(output_error)?;
    }
    if let Some(description) = &preset.metadata.description {
        writeln!(stdout, "description: {description}").map_err(output_error)?;
    }
    if let Some(composition) = composition {
        writeln!(stdout, "composition:").map_err(output_error)?;
        stdout
            .write_all(composition.as_bytes())
            .map_err(output_error)?;
        if !composition.ends_with('\n') {
            stdout.write_all(b"\n").map_err(output_error)?;
        }
    }
    stdout.flush().map_err(output_error)
}

fn write_action(output: ManagementOutput, action: &str, id: &AgentPresetId) -> rsi::Result<()> {
    match output {
        ManagementOutput::Json => write_json(&ActionOutput {
            version: 1,
            kind: "agent_preset_mutation",
            action,
            id: id.as_str(),
        }),
        ManagementOutput::Text => write_text_line(&format!("{action} {}", id.as_str())),
    }
}

fn write_default(output: ManagementOutput, action: &str, id: &AgentPresetId) -> rsi::Result<()> {
    match output {
        ManagementOutput::Json => write_json(&ActionOutput {
            version: 1,
            kind: "agent_preset_default",
            action,
            id: id.as_str(),
        }),
        ManagementOutput::Text if action == "get" => write_text_line(id.as_str()),
        ManagementOutput::Text => write_text_line(&format!("default: {}", id.as_str())),
    }
}

fn write_json(value: &impl Serialize) -> rsi::Result<()> {
    let stdout = std::io::stdout();
    let mut stdout = stdout.lock();
    serde_json::to_writer(&mut stdout, value)
        .map_err(|error| RsiError::Boot(format!("stdout JSON write failed: {error}")))?;
    stdout
        .write_all(b"\n")
        .and_then(|()| stdout.flush())
        .map_err(output_error)
}

fn write_text_line(value: &str) -> rsi::Result<()> {
    let stdout = std::io::stdout();
    let mut stdout = stdout.lock();
    writeln!(stdout, "{value}")
        .and_then(|()| stdout.flush())
        .map_err(output_error)
}

#[allow(clippy::needless_pass_by_value)] // Exact `map_err` adapter for owned I/O failures.
fn output_error(error: std::io::Error) -> RsiError {
    RsiError::Boot(format!("stdout write failed: {error}"))
}

#[allow(clippy::needless_pass_by_value)] // Exact `map_err` adapter for owned catalog failures.
fn preset_management_error(error: PresetError) -> RsiError {
    RsiError::Boot(error.to_string())
}

#[cfg(target_os = "linux")]
fn standard_coding_tools() -> rsi::Result<Option<StandardCodingTools>> {
    let helper = std::env::current_exe()
        .and_then(std::fs::canonicalize)
        .map_err(|error| {
            RsiError::Boot(format!("failed to resolve current executable: {error}"))
        })?;
    let bash = std::fs::canonicalize("/bin/bash")
        .map_err(|error| RsiError::Boot(format!("/bin/bash is unavailable: {error}")))?;
    let environment = scrub_child_environment(std::env::vars_os());
    StandardCodingTools::new(bash, helper, environment).map(Some)
}

#[cfg(not(target_os = "linux"))]
fn standard_coding_tools() -> rsi::Result<Option<StandardCodingTools>> {
    Ok(None)
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
        TurnOutcome::BudgetExceeded {
            dimension,
            consumed,
            limit,
        } => {
            eprintln!("turn budget exceeded for {dimension:?}: consumed {consumed}, limit {limit}");
        }
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
    agent_preset: Option<AgentPresetId>,
    deployment: Option<String>,
    model: Option<String>,
    sandbox: Option<SandboxMode>,
    output: OutputMode,
}

enum Parse {
    Help(&'static str),
    Version,
    Run(Command),
    AgentPreset(AgentPresetCommand),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ManagementOutput {
    Text,
    Json,
}

#[derive(Clone, Debug)]
struct AgentPresetCommand {
    operation: AgentPresetOperation,
    output: ManagementOutput,
}

#[derive(Clone, Debug)]
enum AgentPresetOperation {
    List,
    Show(AgentPresetId),
    Path(AgentPresetId),
    Copy {
        source: AgentPresetId,
        target: AgentPresetId,
        name: Option<String>,
    },
    Delete(AgentPresetId),
    DefaultGet,
    DefaultSet(AgentPresetId),
    DefaultClear,
}

fn parse_agent_preset(arguments: impl Iterator<Item = OsString>) -> rsi::Result<Parse> {
    let arguments = arguments.map(utf8).collect::<rsi::Result<Vec<_>>>()?;
    let Some(command) = arguments.first().map(String::as_str) else {
        return Err(agent_preset_usage("missing agent-preset command"));
    };
    if matches!(command, "-h" | "--help") {
        return Ok(Parse::Help(AGENT_PRESET_HELP));
    }
    let remaining = &arguments[1..];
    if remaining
        .iter()
        .any(|argument| matches!(argument.as_str(), "-h" | "--help"))
    {
        return Ok(Parse::Help(if command == "default" {
            AGENT_PRESET_DEFAULT_HELP
        } else {
            AGENT_PRESET_HELP
        }));
    }
    let parsed = match command {
        "list" => {
            let parsed = management_arguments(remaining, false)?;
            require_positionals(command, &parsed.positionals, 0)?;
            AgentPresetCommand {
                operation: AgentPresetOperation::List,
                output: parsed.output,
            }
        }
        "show" | "path" | "delete" => {
            let parsed = management_arguments(remaining, false)?;
            require_positionals(command, &parsed.positionals, 1)?;
            let id = preset_id(&parsed.positionals[0])?;
            let operation = match command {
                "show" => AgentPresetOperation::Show(id),
                "path" => AgentPresetOperation::Path(id),
                "delete" => AgentPresetOperation::Delete(id),
                _ => unreachable!(),
            };
            AgentPresetCommand {
                operation,
                output: parsed.output,
            }
        }
        "copy" => {
            let parsed = management_arguments(remaining, true)?;
            require_positionals(command, &parsed.positionals, 0)?;
            let source = parsed
                .source
                .as_deref()
                .ok_or_else(|| agent_preset_usage("agent-preset copy requires --from"))?;
            let target = parsed
                .target
                .as_deref()
                .ok_or_else(|| agent_preset_usage("agent-preset copy requires --id"))?;
            AgentPresetCommand {
                operation: AgentPresetOperation::Copy {
                    source: preset_id(source)?,
                    target: preset_id(target)?,
                    name: parsed.name,
                },
                output: parsed.output,
            }
        }
        "default" => parse_default_command(remaining)?,
        _ => {
            return Err(agent_preset_usage(format!(
                "unknown agent-preset command `{command}`"
            )));
        }
    };
    Ok(Parse::AgentPreset(parsed))
}

fn parse_default_command(arguments: &[String]) -> rsi::Result<AgentPresetCommand> {
    let Some(command) = arguments.first().map(String::as_str) else {
        return Err(default_usage("missing agent-preset default command"));
    };
    let parsed = management_arguments(&arguments[1..], false)?;
    let operation = match command {
        "get" => {
            require_default_positionals(command, &parsed.positionals, 0)?;
            AgentPresetOperation::DefaultGet
        }
        "set" => {
            require_default_positionals(command, &parsed.positionals, 1)?;
            AgentPresetOperation::DefaultSet(preset_id(&parsed.positionals[0])?)
        }
        "clear" => {
            require_default_positionals(command, &parsed.positionals, 0)?;
            AgentPresetOperation::DefaultClear
        }
        _ => {
            return Err(default_usage(format!(
                "unknown agent-preset default command `{command}`"
            )));
        }
    };
    Ok(AgentPresetCommand {
        operation,
        output: parsed.output,
    })
}

#[derive(Debug)]
struct ParsedManagementArguments {
    positionals: Vec<String>,
    name: Option<String>,
    source: Option<String>,
    target: Option<String>,
    output: ManagementOutput,
}

fn management_arguments(
    arguments: &[String],
    allow_copy: bool,
) -> rsi::Result<ParsedManagementArguments> {
    let mut positionals = Vec::new();
    let mut name = None;
    let mut source = None;
    let mut target = None;
    let mut output = ManagementOutput::Text;
    let mut output_set = false;
    let mut index = 0;
    while index < arguments.len() {
        match arguments[index].as_str() {
            "--output" => {
                if output_set {
                    return Err(agent_preset_usage("duplicate --output"));
                }
                output_set = true;
                index += 1;
                let value = arguments
                    .get(index)
                    .ok_or_else(|| agent_preset_usage("--output requires a value"))?;
                output = match value.as_str() {
                    "text" => ManagementOutput::Text,
                    "json" => ManagementOutput::Json,
                    _ => return Err(agent_preset_usage("invalid --output mode")),
                };
            }
            "--name" if allow_copy => {
                if name.is_some() {
                    return Err(agent_preset_usage("duplicate --name"));
                }
                index += 1;
                name = Some(
                    arguments
                        .get(index)
                        .ok_or_else(|| agent_preset_usage("--name requires a value"))?
                        .clone(),
                );
            }
            "--from" if allow_copy => {
                if source.is_some() {
                    return Err(agent_preset_usage("duplicate --from"));
                }
                index += 1;
                source = Some(
                    arguments
                        .get(index)
                        .ok_or_else(|| agent_preset_usage("--from requires a value"))?
                        .clone(),
                );
            }
            "--id" if allow_copy => {
                if target.is_some() {
                    return Err(agent_preset_usage("duplicate --id"));
                }
                index += 1;
                target = Some(
                    arguments
                        .get(index)
                        .ok_or_else(|| agent_preset_usage("--id requires a value"))?
                        .clone(),
                );
            }
            option if option.starts_with('-') => {
                return Err(agent_preset_usage(format!("unknown option `{option}`")));
            }
            positional => positionals.push(positional.to_owned()),
        }
        index += 1;
    }
    Ok(ParsedManagementArguments {
        positionals,
        name,
        source,
        target,
        output,
    })
}

fn require_positionals(command: &str, values: &[String], expected: usize) -> rsi::Result<()> {
    if values.len() == expected {
        Ok(())
    } else {
        Err(agent_preset_usage(format!(
            "agent-preset {command} expects {expected} positional argument(s)"
        )))
    }
}

fn require_default_positionals(
    command: &str,
    values: &[String],
    expected: usize,
) -> rsi::Result<()> {
    if values.len() == expected {
        Ok(())
    } else {
        Err(default_usage(format!(
            "agent-preset default {command} expects {expected} positional argument(s)"
        )))
    }
}

fn preset_id(value: &str) -> rsi::Result<AgentPresetId> {
    AgentPresetId::new(value).map_err(|error| agent_preset_usage(error.to_string()))
}

impl Command {
    fn empty() -> Self {
        Self {
            positional: None,
            stdin: false,
            profile: None,
            cwd: None,
            resume: None,
            session_id: None,
            agent_preset: None,
            deployment: None,
            model: None,
            sandbox: None,
            output: OutputMode::Text,
        }
    }

    fn parse(arguments: impl IntoIterator<Item = OsString>) -> rsi::Result<Parse> {
        let mut arguments = arguments.into_iter();
        let Some(first) = arguments.next() else {
            return Err(usage("missing `run` command"));
        };
        let first = utf8(first)?;
        if matches!(first.as_str(), "-h" | "--help") {
            return Ok(Parse::Help(HELP));
        }
        if matches!(first.as_str(), "-V" | "--version") {
            return Ok(Parse::Version);
        }
        if first == "agent-preset" {
            return parse_agent_preset(arguments);
        }
        if first != "run" {
            return Err(usage(
                "only the `run` command is supported for turns; `agent-preset` is the management command",
            ));
        }

        let mut command = Self::empty();
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
                    "--agent-preset" => set_option(
                        &mut command.agent_preset,
                        run_preset_value(&mut arguments)?,
                        "--agent-preset",
                    )?,
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
                    "-h" | "--help" => return Ok(Parse::Help(HELP)),
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
        if self.resume.is_some() && self.agent_preset.is_some() {
            return Err(usage("--resume and --agent-preset are mutually exclusive"));
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
                agent_preset_id: self.agent_preset.clone(),
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

fn run_preset_value(arguments: &mut impl Iterator<Item = OsString>) -> rsi::Result<AgentPresetId> {
    let value = string_value(arguments, "--agent-preset")?;
    AgentPresetId::new(value).map_err(|error| usage(error.to_string()))
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

fn agent_preset_usage(message: impl Into<String>) -> RsiError {
    RsiError::Boot(format!("{}\n{AGENT_PRESET_HELP}", message.into()))
}

fn default_usage(message: impl Into<String>) -> RsiError {
    RsiError::Boot(format!("{}\n{AGENT_PRESET_DEFAULT_HELP}", message.into()))
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
    fn parses_one_valid_agent_preset_only_for_a_fresh_session() {
        let Parse::Run(command) =
            parse(&["run", "task", "--agent-preset", "coding-agent"]).unwrap()
        else {
            panic!("run")
        };
        assert_eq!(
            command.agent_preset.as_ref().map(AgentPresetId::as_str),
            Some("coding-agent")
        );
        let options = command.options("task".into()).unwrap();
        assert!(matches!(
            options.session,
            SessionSelection::Fresh {
                agent_preset_id: Some(ref id),
                ..
            } if id.as_str() == "coding-agent"
        ));
        assert!(
            parse(&[
                "run",
                "task",
                "--agent-preset",
                "coding-agent",
                "--agent-preset",
                "review-agent"
            ])
            .is_err()
        );
        assert!(parse(&["run", "task", "--agent-preset", "Upper"]).is_err());
        assert!(
            parse(&[
                "run",
                "task",
                "--resume",
                "session-one",
                "--agent-preset",
                "coding-agent"
            ])
            .is_err()
        );
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
