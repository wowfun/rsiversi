use super::*;

pub(super) const HELP: &str = "Usage:\n\
  rsi --profile PROFILE [APPLICATION ARGUMENTS]\n\
      headless: TASK|--stdin [--cwd PATH] [--resume SESSION|--session-id SESSION]\n\
                [--message-id MESSAGE] [-i|--image PATH]... [--agent-preset ID]\n\
                [--deployment ID --model ID] [--sandbox MODE]\n\
                [--trust-workspace] [--output text|jsonl]\n\
      session:  [--cwd PATH] [--resume SESSION|--session-id SESSION]\n\
                [--agent-preset ID] [--trust-workspace] [--output text|jsonl]\n\
  rsi profile <application|host> <COMMAND> [--output text|json]\n\
  rsi host <start|serve|restart|stop|status|reload> [--profile HOST]\n\
  rsi agent-preset <COMMAND> [--output text|json]\n\
  rsi agent-store verify [--root ABSOLUTE] [--output text|json]\n\n\
Commands:\n\
  --profile       Run a named Session or headless Application Profile\n\
  profile         Inspect and manage Application and Host Profiles\n\
  host            Control the explicit local Session Host daemon\n\
  agent-preset    Inspect and manage local Agent presets\n\
  agent-store     Verify the durable Agent Store\n";
pub(super) const PROFILE_HELP: &str = "Usage:\n\
  rsi profile <application|host> list [--output text|json]\n\
  rsi profile <application|host> show ID [--output text|json]\n\
  rsi profile <application|host> path ID [--output text|json]\n\
  rsi profile <application|host> copy FROM TO [--output text|json]\n\
  rsi profile <application|host> delete ID [--output text|json]\n\
  rsi profile host preview ID [--output text|json]\n";
pub(super) const HOST_HELP: &str = "Usage:\n\
  rsi host start [--profile HOST]\n\
  rsi host serve [--profile HOST]\n\
  rsi host restart [--profile HOST] [--force]\n\
  rsi host stop [--force]\n\
  rsi host status\n\
  rsi host reload\n";
pub(super) const AGENT_PRESET_HELP: &str = "Usage:\n\
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
pub(super) const AGENT_PRESET_DEFAULT_HELP: &str = "Usage:\n\
  rsi agent-preset default get [--output text|json]\n\
  rsi agent-preset default set ID [--output text|json]\n\
  rsi agent-preset default clear [--output text|json]\n\n\
Commands:\n\
  get      Print the effective default\n\
  set      Store one syntactically valid preset id\n\
  clear    Re-inherit the deployment default\n";
pub(super) const AGENT_STORE_HELP: &str = "Usage:\n\
  rsi agent-store verify [--root ABSOLUTE] [--output text|json]\n\n\
Commands:\n\
  verify    Run an offline full integrity audit without creating a Store\n";
pub(super) const BOOT_FAILURE_EXIT_CODE: u8 = 2;

#[derive(Clone, Debug)]
pub(super) struct Command {
    pub(super) positional: Option<String>,
    pub(super) stdin: bool,
    pub(super) cwd: Option<PathBuf>,
    pub(super) resume: Option<SessionId>,
    pub(super) session_id: Option<SessionId>,
    pub(super) message_id: Option<MessageId>,
    pub(super) images: Vec<PathBuf>,
    pub(super) agent_preset: Option<AgentPresetId>,
    pub(super) trust_workspace: bool,
    pub(super) deployment: Option<String>,
    pub(super) model: Option<String>,
    pub(super) sandbox: Option<SandboxMode>,
    pub(super) output: OutputMode,
}

pub(super) enum Parse {
    Help(&'static str),
    Version,
    Application(ApplicationInvocation),
    Profile(ProfileCommand),
    Host(HostCommand),
    Run(Command),
    AgentPreset(AgentPresetCommand),
    AgentStore(AgentStoreCommand),
}

#[derive(Clone, Debug)]
pub(super) struct ApplicationInvocation {
    pub(super) profile: ApplicationProfileId,
    pub(super) arguments: Vec<OsString>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum ProfileKind {
    Application,
    Host,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum ProfileOperationKind {
    List,
    Show,
    Path,
    Copy,
    Delete,
    Preview,
}

#[derive(Clone, Debug)]
pub(super) struct ProfileCommand {
    pub(super) kind: ProfileKind,
    pub(super) operation: ProfileOperationKind,
    pub(super) ids: Vec<String>,
    pub(super) output: ManagementOutput,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum HostOperation {
    Start,
    Serve,
    Restart,
    Stop,
    Status,
    Reload,
}

#[derive(Clone, Debug)]
pub(super) struct HostCommand {
    pub(super) operation: HostOperation,
    pub(super) profile: HostProfileId,
    pub(super) force: bool,
    pub(super) detached_child: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum ManagementOutput {
    Text,
    Json,
}

#[derive(Clone, Debug)]
pub(super) struct AgentPresetCommand {
    pub(super) operation: AgentPresetOperation,
    pub(super) output: ManagementOutput,
}

#[derive(Clone, Debug)]
pub(super) struct AgentStoreCommand {
    pub(super) root: Option<PathBuf>,
    pub(super) output: ManagementOutput,
}

#[derive(Clone, Debug)]
pub(super) enum AgentPresetOperation {
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

pub(super) fn parse_agent_store(arguments: impl Iterator<Item = OsString>) -> rsi::Result<Parse> {
    let arguments = arguments.map(utf8).collect::<rsi::Result<Vec<_>>>()?;
    let Some(command) = arguments.first().map(String::as_str) else {
        return Err(agent_store_usage("missing agent-store command"));
    };
    if arguments
        .iter()
        .any(|argument| matches!(argument.as_str(), "-h" | "--help"))
    {
        return Ok(Parse::Help(AGENT_STORE_HELP));
    }
    if command != "verify" {
        return Err(agent_store_usage(format!(
            "unknown agent-store command `{command}`"
        )));
    }
    let mut root = None;
    let mut output = ManagementOutput::Text;
    let mut output_set = false;
    let mut index = 1;
    while index < arguments.len() {
        match arguments[index].as_str() {
            "--root" => {
                if root.is_some() {
                    return Err(agent_store_usage("duplicate --root"));
                }
                index += 1;
                let value = arguments
                    .get(index)
                    .ok_or_else(|| agent_store_usage("--root requires a value"))?;
                let path = PathBuf::from(value);
                if !path.is_absolute() {
                    return Err(agent_store_usage("--root must be absolute"));
                }
                root = Some(path);
            }
            "--output" => {
                if output_set {
                    return Err(agent_store_usage("duplicate --output"));
                }
                output_set = true;
                index += 1;
                output = match arguments.get(index).map(String::as_str) {
                    Some("text") => ManagementOutput::Text,
                    Some("json") => ManagementOutput::Json,
                    Some(_) => return Err(agent_store_usage("invalid --output mode")),
                    None => return Err(agent_store_usage("--output requires a value")),
                };
            }
            option if option.starts_with('-') => {
                return Err(agent_store_usage(format!("unknown option `{option}`")));
            }
            positional => {
                return Err(agent_store_usage(format!(
                    "unexpected positional argument `{positional}`"
                )));
            }
        }
        index += 1;
    }
    Ok(Parse::AgentStore(AgentStoreCommand { root, output }))
}

pub(super) fn parse_agent_preset(arguments: impl Iterator<Item = OsString>) -> rsi::Result<Parse> {
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

pub(super) fn parse_default_command(arguments: &[String]) -> rsi::Result<AgentPresetCommand> {
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
pub(super) struct ParsedManagementArguments {
    positionals: Vec<String>,
    name: Option<String>,
    source: Option<String>,
    target: Option<String>,
    output: ManagementOutput,
}

pub(super) fn management_arguments(
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

pub(super) fn require_positionals(
    command: &str,
    values: &[String],
    expected: usize,
) -> rsi::Result<()> {
    if values.len() == expected {
        Ok(())
    } else {
        Err(agent_preset_usage(format!(
            "agent-preset {command} expects {expected} positional argument(s)"
        )))
    }
}

pub(super) fn require_default_positionals(
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

pub(super) fn preset_id(value: &str) -> rsi::Result<AgentPresetId> {
    AgentPresetId::new(value).map_err(|error| agent_preset_usage(error.to_string()))
}

pub(super) fn parse_profile_command(
    arguments: impl Iterator<Item = OsString>,
) -> rsi::Result<Parse> {
    let arguments = arguments.map(utf8).collect::<rsi::Result<Vec<_>>>()?;
    if arguments
        .iter()
        .any(|argument| matches!(argument.as_str(), "-h" | "--help"))
    {
        return Ok(Parse::Help(PROFILE_HELP));
    }
    let kind = match arguments.first().map(String::as_str) {
        Some("application") => ProfileKind::Application,
        Some("host") => ProfileKind::Host,
        Some(value) => return Err(profile_usage(format!("unknown Profile kind `{value}`"))),
        None => return Err(profile_usage("missing Profile kind")),
    };
    let operation = match arguments.get(1).map(String::as_str) {
        Some("list") => ProfileOperationKind::List,
        Some("show") => ProfileOperationKind::Show,
        Some("path") => ProfileOperationKind::Path,
        Some("copy") => ProfileOperationKind::Copy,
        Some("delete") => ProfileOperationKind::Delete,
        Some("preview") if kind == ProfileKind::Host => ProfileOperationKind::Preview,
        Some(value) => return Err(profile_usage(format!("unknown Profile command `{value}`"))),
        None => return Err(profile_usage("missing Profile command")),
    };
    let mut ids = Vec::new();
    let mut output = ManagementOutput::Text;
    let mut output_set = false;
    let mut index = 2;
    while index < arguments.len() {
        match arguments[index].as_str() {
            "--output" => {
                if output_set {
                    return Err(profile_usage("duplicate --output"));
                }
                output_set = true;
                index += 1;
                output = match arguments.get(index).map(String::as_str) {
                    Some("text") => ManagementOutput::Text,
                    Some("json") => ManagementOutput::Json,
                    Some(_) => return Err(profile_usage("invalid --output mode")),
                    None => return Err(profile_usage("--output requires a value")),
                };
            }
            option if option.starts_with('-') => {
                return Err(profile_usage(format!("unknown option `{option}`")));
            }
            id => ids.push(id.into()),
        }
        index += 1;
    }
    let expected = match operation {
        ProfileOperationKind::List => 0,
        ProfileOperationKind::Show
        | ProfileOperationKind::Path
        | ProfileOperationKind::Delete
        | ProfileOperationKind::Preview => 1,
        ProfileOperationKind::Copy => 2,
    };
    if ids.len() != expected {
        return Err(profile_usage(format!(
            "Profile command expects {expected} identifier(s)"
        )));
    }
    Ok(Parse::Profile(ProfileCommand {
        kind,
        operation,
        ids,
        output,
    }))
}

pub(super) fn parse_host_command(arguments: impl Iterator<Item = OsString>) -> rsi::Result<Parse> {
    let arguments = arguments.map(utf8).collect::<rsi::Result<Vec<_>>>()?;
    if arguments
        .iter()
        .any(|argument| matches!(argument.as_str(), "-h" | "--help"))
    {
        return Ok(Parse::Help(HOST_HELP));
    }
    let operation = match arguments.first().map(String::as_str) {
        Some("start") => HostOperation::Start,
        Some("serve") => HostOperation::Serve,
        Some("restart") => HostOperation::Restart,
        Some("stop") => HostOperation::Stop,
        Some("status") => HostOperation::Status,
        Some("reload") => HostOperation::Reload,
        Some(value) => return Err(host_usage(format!("unknown Host command `{value}`"))),
        None => return Err(host_usage("missing Host command")),
    };
    let mut profile =
        HostProfileId::new("standard").map_err(|error| host_usage(error.to_string()))?;
    let mut profile_set = false;
    let mut force = false;
    let mut detached_child = false;
    let mut index = 1;
    while index < arguments.len() {
        match arguments[index].as_str() {
            "--profile" => {
                if profile_set {
                    return Err(host_usage("duplicate --profile"));
                }
                profile_set = true;
                index += 1;
                profile = HostProfileId::new(
                    arguments
                        .get(index)
                        .ok_or_else(|| host_usage("--profile requires a value"))?
                        .clone(),
                )
                .map_err(|error| host_usage(error.to_string()))?;
            }
            "--force" => {
                if force {
                    return Err(host_usage("duplicate --force"));
                }
                force = true;
            }
            "--detached-child" => {
                if detached_child {
                    return Err(host_usage("duplicate --detached-child"));
                }
                detached_child = true;
            }
            option => return Err(host_usage(format!("unknown Host option `{option}`"))),
        }
        index += 1;
    }
    if profile_set
        && matches!(
            operation,
            HostOperation::Stop | HostOperation::Status | HostOperation::Reload
        )
    {
        return Err(host_usage("this Host command does not select a Profile"));
    }
    if force && !matches!(operation, HostOperation::Stop | HostOperation::Restart) {
        return Err(host_usage("--force is valid only for stop or restart"));
    }
    if detached_child && operation != HostOperation::Serve {
        return Err(host_usage(
            "--detached-child is valid only for the internal serve child",
        ));
    }
    Ok(Parse::Host(HostCommand {
        operation,
        profile,
        force,
        detached_child,
    }))
}

impl Command {
    pub(super) fn parse_cli(arguments: impl IntoIterator<Item = OsString>) -> rsi::Result<Parse> {
        let arguments = arguments.into_iter().collect::<Vec<_>>();
        if arguments.first().and_then(|argument| argument.to_str()) == Some("run") {
            return Err(usage(
                "the direct `run` command was removed; select a named Application Profile with `rsi --profile headless ...`",
            ));
        }
        Self::parse(arguments)
    }

    fn empty() -> Self {
        Self {
            positional: None,
            stdin: false,
            cwd: None,
            resume: None,
            session_id: None,
            message_id: None,
            images: Vec::new(),
            agent_preset: None,
            trust_workspace: false,
            deployment: None,
            model: None,
            sandbox: None,
            output: OutputMode::Text,
        }
    }

    #[allow(clippy::too_many_lines)] // One ordered CLI grammar owns option conflicts and exact diagnostics.
    pub(super) fn parse(arguments: impl IntoIterator<Item = OsString>) -> rsi::Result<Parse> {
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
        if first == "--profile" {
            let profile = arguments
                .next()
                .ok_or_else(|| usage("--profile requires an Application Profile name"))?;
            let profile = ApplicationProfileId::new(utf8(profile)?)
                .map_err(|error| usage(error.to_string()))?;
            return Ok(Parse::Application(ApplicationInvocation {
                profile,
                arguments: arguments.collect(),
            }));
        }
        if first == "profile" {
            return parse_profile_command(arguments);
        }
        if first == "host" {
            return parse_host_command(arguments);
        }
        if first == "agent-preset" {
            return parse_agent_preset(arguments);
        }
        if first == "agent-store" {
            return parse_agent_store(arguments);
        }
        if first != "run" {
            return Err(usage(format!("unknown command `{first}`")));
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
                    "--message-id" => set_option(
                        &mut command.message_id,
                        message_value(&mut arguments, "--message-id")?,
                        "--message-id",
                    )?,
                    "-i" | "--image" => {
                        command.images.push(path_value(&mut arguments, &argument)?);
                    }
                    "--agent-preset" => set_option(
                        &mut command.agent_preset,
                        run_preset_value(&mut arguments)?,
                        "--agent-preset",
                    )?,
                    "--trust-workspace" => {
                        set_flag(&mut command.trust_workspace, "--trust-workspace")?;
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
        if self.resume.is_some() && self.trust_workspace {
            return Err(usage(
                "--trust-workspace cannot change an existing Session's immutable authority",
            ));
        }
        if self.deployment.is_some() != self.model.is_some() {
            return Err(usage("--deployment and --model must be supplied together"));
        }
        if let (Some(deployment), Some(model)) = (&self.deployment, &self.model) {
            ModelRef::new(deployment, model).map_err(|error| usage(error.to_string()))?;
        }
        Ok(())
    }

    pub(super) async fn task(&self) -> rsi::Result<String> {
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

    pub(super) fn options(&self, task: String) -> rsi::Result<HeadlessTurnOptions> {
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
                workspace_trust: if self.trust_workspace {
                    WorkspaceTrust::Trusted
                } else {
                    WorkspaceTrust::Untrusted
                },
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
        Ok(HeadlessTurnOptions {
            task,
            session,
            message_id: self.message_id.clone(),
            images: self.images.clone(),
            model,
            sandbox: self.sandbox,
            output: self.output,
        })
    }
}

pub(super) fn sandbox_value(
    arguments: &mut impl Iterator<Item = OsString>,
) -> rsi::Result<SandboxMode> {
    match string_value(arguments, "--sandbox")?.as_str() {
        "read-only" => Ok(SandboxMode::ReadOnly),
        "workspace-write" => Ok(SandboxMode::WorkspaceWrite),
        "danger-full-access" => Ok(SandboxMode::DangerFullAccess),
        _ => Err(usage("invalid --sandbox mode")),
    }
}

pub(super) fn output_value(
    arguments: &mut impl Iterator<Item = OsString>,
) -> rsi::Result<OutputMode> {
    match string_value(arguments, "--output")?.as_str() {
        "text" => Ok(OutputMode::Text),
        "jsonl" => Ok(OutputMode::Jsonl),
        _ => Err(usage("invalid --output mode")),
    }
}

pub(super) fn run_preset_value(
    arguments: &mut impl Iterator<Item = OsString>,
) -> rsi::Result<AgentPresetId> {
    let value = string_value(arguments, "--agent-preset")?;
    AgentPresetId::new(value).map_err(|error| usage(error.to_string()))
}

pub(super) fn set_flag(value: &mut bool, name: &str) -> rsi::Result<()> {
    if *value {
        return Err(usage(format!("duplicate {name}")));
    }
    *value = true;
    Ok(())
}

pub(super) fn set_option<T>(slot: &mut Option<T>, value: T, name: &str) -> rsi::Result<()> {
    if slot.is_some() {
        return Err(usage(format!("duplicate {name}")));
    }
    *slot = Some(value);
    Ok(())
}

pub(super) fn path_value(
    arguments: &mut impl Iterator<Item = OsString>,
    option: &str,
) -> rsi::Result<PathBuf> {
    arguments
        .next()
        .map(PathBuf::from)
        .ok_or_else(|| usage(format!("{option} requires a value")))
}

pub(super) fn session_value(
    arguments: &mut impl Iterator<Item = OsString>,
    option: &str,
) -> rsi::Result<SessionId> {
    let value = string_value(arguments, option)?;
    SessionId::new(value).map_err(|error| usage(error.to_string()))
}

pub(super) fn message_value(
    arguments: &mut impl Iterator<Item = OsString>,
    option: &str,
) -> rsi::Result<MessageId> {
    let value = string_value(arguments, option)?;
    MessageId::new(value).map_err(|error| usage(error.to_string()))
}

pub(super) fn string_value(
    arguments: &mut impl Iterator<Item = OsString>,
    option: &str,
) -> rsi::Result<String> {
    let value = arguments
        .next()
        .ok_or_else(|| usage(format!("{option} requires a value")))?;
    utf8(value)
}

pub(super) fn utf8(value: OsString) -> rsi::Result<String> {
    value
        .into_string()
        .map_err(|_| usage("CLI arguments must be UTF-8"))
}

pub(super) fn usage(message: impl Into<String>) -> RsiError {
    RsiError::Boot(format!("{}\n{HELP}", message.into()))
}

pub(super) fn agent_preset_usage(message: impl Into<String>) -> RsiError {
    RsiError::Boot(format!("{}\n{AGENT_PRESET_HELP}", message.into()))
}

pub(super) fn default_usage(message: impl Into<String>) -> RsiError {
    RsiError::Boot(format!("{}\n{AGENT_PRESET_DEFAULT_HELP}", message.into()))
}

pub(super) fn agent_store_usage(message: impl Into<String>) -> RsiError {
    RsiError::Boot(format!("{}\n{AGENT_STORE_HELP}", message.into()))
}

pub(super) fn profile_usage(message: impl Into<String>) -> RsiError {
    RsiError::Boot(format!("{}\n{PROFILE_HELP}", message.into()))
}

pub(super) fn host_usage(message: impl Into<String>) -> RsiError {
    RsiError::Boot(format!("{}\n{HOST_HELP}", message.into()))
}
