use super::{AgentPresetId, ApplicationProfileId, HostProfileId, OsString, PathBuf, RsiError};
use rsi_application::arguments::utf8;

pub(super) const HELP: &str = "Usage:\n\
  rsi --profile PROFILE [APPLICATION ARGUMENTS]\n\
      headless: TASK|--stdin [--cwd PATH] [--resume SESSION|--session-id SESSION]\n\
                [--message-id MESSAGE] [-i|--image PATH]... [--agent-preset ID]\n\
                [--deployment ID --model ID] [--sandbox MODE]\n\
                [--trust-workspace] [--output text|jsonl]\n\
      cli:  [--cwd PATH] [--resume SESSION|--history SESSION|--list|--session-id SESSION]\n\
                [--agent-preset ID] [--trust-workspace] [--output text|jsonl]\n\
      tui:  [--cwd PATH] [--resume SESSION|--session-id SESSION]\n\
      serve: --bind ADDRESS --origin ORIGIN [--tls-certificate FILE --tls-key FILE|--dev-http]\n\
      devices: <register LABEL|list|revoke DEVICE_ID>\n\
  rsi profile <application|host> <COMMAND> [--output text|json]\n\
  rsi host <start|serve|restart|stop|status|reload> [--profile HOST]\n\
  rsi agent-preset <COMMAND> [--output text|json]\n\
  rsi agent-store verify [--root ABSOLUTE] [--output text|json]\n\n\
Commands:\n\
  --profile       Run a named application plugin Profile\n\
  profile         Inspect and manage Application and Host Profiles\n\
  host            Control the explicit local Service Host daemon\n\
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

pub(super) enum Parse {
    Help(&'static str),
    Version,
    Application(ApplicationInvocation),
    Profile(ProfileCommand),
    #[cfg(target_os = "linux")]
    Host(HostCommand),
    #[cfg(not(target_os = "linux"))]
    HostUnsupported,
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
#[cfg(target_os = "linux")]
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
    #[cfg(target_os = "linux")]
    {
        Ok(Parse::Host(HostCommand {
            operation,
            profile,
            force,
            detached_child,
        }))
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _validated = (operation, profile, force, detached_child);
        Ok(Parse::HostUnsupported)
    }
}

pub(super) fn parse_cli(arguments: impl IntoIterator<Item = OsString>) -> rsi::Result<Parse> {
    let mut arguments = arguments.into_iter();
    let Some(first) = arguments.next() else {
        return Err(usage("missing application selection or management command"));
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
        let profile =
            ApplicationProfileId::new(utf8(profile)?).map_err(|error| usage(error.to_string()))?;
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
    Err(usage(format!(
        "unknown command `{first}`; select an Application Profile with --profile"
    )))
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
