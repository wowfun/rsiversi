use super::*;
use rsi_application::arguments::string_value;
const HELP: &str = "Use rsi --profile headless --help for application options.";

#[derive(Clone, Debug)]
pub(crate) struct Command {
    pub(crate) extension: Option<crate::headless_commands::Extension>,
    pub(crate) positional: Option<String>,
    pub(crate) stdin: bool,
    pub(crate) cwd: Option<PathBuf>,
    pub(crate) resume: Option<SessionId>,
    pub(crate) session_id: Option<SessionId>,
    pub(crate) message_id: Option<MessageId>,
    pub(crate) images: Vec<PathBuf>,
    pub(crate) agent_preset: Option<AgentPresetId>,
    pub(crate) trust_workspace: bool,
    pub(crate) deployment: Option<String>,
    pub(crate) model: Option<String>,
    pub(crate) sandbox: Option<SandboxMode>,
    pub(crate) output: OutputMode,
}

impl Command {
    fn empty() -> Self {
        Self {
            extension: None,
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

    #[allow(clippy::too_many_lines)] // One application grammar owns all option conflicts.
    pub(crate) fn parse(arguments: impl IntoIterator<Item = OsString>) -> Result<Self> {
        let mut arguments = arguments.into_iter();
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
                    "--commands" | "--command" | "--command-status" => {
                        let extension =
                            crate::headless_commands::Extension::parse(&argument, &mut arguments)?;
                        set_option(
                            &mut command.extension,
                            extension,
                            "Session command operation",
                        )?;
                    }
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
                    "-h" | "--help" => {
                        return Err(usage("help is handled before application preparation"));
                    }
                    _ => return Err(usage(format!("unknown option `{argument}`"))),
                }
            } else if command.positional.replace(argument).is_some() {
                return Err(usage("exactly one task positional is allowed"));
            }
        }
        command.validate()?;
        Ok(command)
    }

    fn validate(&self) -> Result<()> {
        let has_task = self.stdin || self.positional.is_some();
        if self.stdin && self.positional.is_some() || !has_task && self.extension.is_none() {
            return Err(usage("provide exactly one task positional or --stdin"));
        }
        if let Some(extension) = &self.extension {
            extension.validate(has_task)?;
            if !has_task
                && (!self.images.is_empty()
                    || self.message_id.is_some()
                    || self.deployment.is_some()
                    || self.model.is_some()
                    || self.sandbox.is_some())
            {
                return Err(usage(
                    "message, image, model and sandbox options require a task",
                ));
            }
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

    pub(crate) async fn task(&self, work: &ApplicationWork) -> Result<String> {
        if let Some(task) = &self.positional {
            return Ok(task.clone());
        }
        if !self.stdin {
            return Ok(String::new());
        }
        let stop = work.stop.clone();
        let token = work.tasks.token();
        let input = tokio::task::spawn_blocking(move || {
            let _token = token;
            let mut input = Vec::new();
            crate::work::stdin(stop)?
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

    pub(crate) fn options(&self, task: String) -> Result<HeadlessTurnOptions> {
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

pub(crate) fn sandbox_value(arguments: &mut impl Iterator<Item = OsString>) -> Result<SandboxMode> {
    match string_value(arguments, "--sandbox")?.as_str() {
        "read-only" => Ok(SandboxMode::ReadOnly),
        "workspace-write" => Ok(SandboxMode::WorkspaceWrite),
        "danger-full-access" => Ok(SandboxMode::DangerFullAccess),
        _ => Err(usage("invalid --sandbox mode")),
    }
}

pub(crate) fn output_value(arguments: &mut impl Iterator<Item = OsString>) -> Result<OutputMode> {
    match string_value(arguments, "--output")?.as_str() {
        "text" => Ok(OutputMode::Text),
        "jsonl" => Ok(OutputMode::Jsonl),
        _ => Err(usage("invalid --output mode")),
    }
}

pub(crate) fn run_preset_value(
    arguments: &mut impl Iterator<Item = OsString>,
) -> Result<AgentPresetId> {
    let value = string_value(arguments, "--agent-preset")?;
    AgentPresetId::new(value).map_err(|error| usage(error.to_string()))
}

pub(crate) fn session_value(
    arguments: &mut impl Iterator<Item = OsString>,
    option: &str,
) -> Result<SessionId> {
    let value = string_value(arguments, option)?;
    SessionId::new(value).map_err(|error| usage(error.to_string()))
}

pub(crate) fn message_value(
    arguments: &mut impl Iterator<Item = OsString>,
    option: &str,
) -> Result<MessageId> {
    let value = string_value(arguments, option)?;
    MessageId::new(value).map_err(|error| usage(error.to_string()))
}

pub(crate) fn usage(message: impl Into<String>) -> RsiError {
    RsiError::Boot(format!("{}\n{HELP}", message.into()))
}
