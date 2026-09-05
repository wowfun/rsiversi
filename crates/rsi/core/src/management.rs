use super::{
    AgentPresetCommand, AgentPresetHealth, AgentPresetId, AgentPresetManager, AgentPresetOperation,
    AgentPresetRow, AgentPresetSource, AgentPresetTrust, AgentStoreCommand, ApplicationProfileId,
    BTreeMap, HostProfileId, ManagementOutput, PresetError, ProfileCatalog, ProfileCommand,
    ProfileKind, ProfileOperationKind, ProfileSource, RsiError, Serialize, SqliteStore,
    StandardComposition, Write, report_error, standard_agent_preset_root, standard_coding_tools,
    standard_paths,
};

pub(super) async fn run_agent_store(command: AgentStoreCommand) -> u8 {
    let root = match command.root {
        Some(root) => root,
        None => match standard_paths() {
            Ok(paths) => paths.state().join("agent"),
            Err(error) => return report_error(&error),
        },
    };
    let verify_root = root.clone();
    let verification = tokio::task::spawn_blocking(move || SqliteStore::verify(verify_root)).await;
    match verification {
        Ok(Ok(())) => {
            let output = match command.output {
                ManagementOutput::Text => write_text_line(&format!("verified\t{}", root.display())),
                ManagementOutput::Json => write_json(&AgentStoreVerifyOutput {
                    version: 1,
                    kind: "agent_store_verify",
                    status: "ok",
                    root: &root,
                }),
            };
            output.map_or_else(|error| report_error(&error), |()| 0)
        }
        Ok(Err(error)) => report_error(&RsiError::Boot(format!(
            "Agent Store verification failed: {error}"
        ))),
        Err(error) => report_error(&RsiError::Boot(format!(
            "Agent Store verification worker failed: {error}"
        ))),
    }
}

#[allow(clippy::too_many_lines)] // One closed Profile command matrix owns all output variants.
pub(super) async fn run_profile(command: &ProfileCommand) -> u8 {
    if matches!(
        (command.kind, command.operation),
        (ProfileKind::Host, ProfileOperationKind::Preview)
    ) {
        return run_host_profile_preview(command).await;
    }
    let result = (|| -> rsi::Result<()> {
        let paths = standard_paths()?;
        let catalog = ProfileCatalog::new(paths.clone());
        match (command.kind, command.operation) {
            (ProfileKind::Application, ProfileOperationKind::List) => {
                let rows = catalog
                    .list_applications()
                    .map_err(profile_management_error)?;
                write_profile_rows(
                    command.output,
                    "application_profile_list",
                    rows.into_iter()
                        .map(|row| (row.id.to_string(), row.source))
                        .collect(),
                )
            }
            (ProfileKind::Host, ProfileOperationKind::List) => {
                let rows = catalog.list_hosts().map_err(profile_management_error)?;
                write_profile_rows(
                    command.output,
                    "host_profile_list",
                    rows.into_iter()
                        .map(|row| (row.id.to_string(), row.source))
                        .collect(),
                )
            }
            (ProfileKind::Application, ProfileOperationKind::Show) => {
                let id = application_profile_id(&command.ids[0])?;
                let document = catalog.application(&id).map_err(profile_management_error)?;
                let contents = toml::to_string_pretty(&document.profile)
                    .map_err(|error| RsiError::Boot(error.to_string()))?;
                write_profile_document(
                    command.output,
                    "application_profile",
                    id.as_str(),
                    document.source,
                    document.path.as_deref(),
                    &contents,
                )
            }
            (ProfileKind::Host, ProfileOperationKind::Show) => {
                let id = host_profile_id(&command.ids[0])?;
                let document = catalog.host(&id).map_err(profile_management_error)?;
                let contents = std::str::from_utf8(&document.contents)
                    .map_err(|_| RsiError::Boot("Host Profile source is not valid UTF-8".into()))?;
                write_profile_document(
                    command.output,
                    "host_profile",
                    id.as_str(),
                    document.source,
                    document.path.as_deref(),
                    contents,
                )
            }
            (ProfileKind::Application, ProfileOperationKind::Path) => {
                let id = application_profile_id(&command.ids[0])?;
                let document = catalog.application(&id).map_err(profile_management_error)?;
                write_profile_path(
                    command.output,
                    "application_profile_path",
                    id.as_str(),
                    document.source,
                    document.path.as_deref(),
                )
            }
            (ProfileKind::Host, ProfileOperationKind::Path) => {
                let id = host_profile_id(&command.ids[0])?;
                let document = catalog.host(&id).map_err(profile_management_error)?;
                write_profile_path(
                    command.output,
                    "host_profile_path",
                    id.as_str(),
                    document.source,
                    document.path.as_deref(),
                )
            }
            (ProfileKind::Application, ProfileOperationKind::Copy) => {
                let source = application_profile_id(&command.ids[0])?;
                let target = application_profile_id(&command.ids[1])?;
                let path = catalog
                    .copy_application(&source, &target)
                    .map_err(profile_management_error)?;
                write_profile_mutation(
                    command.output,
                    "application_profile",
                    "copied",
                    &target,
                    &path,
                )
            }
            (ProfileKind::Host, ProfileOperationKind::Copy) => {
                let source = host_profile_id(&command.ids[0])?;
                let target = host_profile_id(&command.ids[1])?;
                let path = catalog
                    .copy_host(&source, &target)
                    .map_err(profile_management_error)?;
                write_profile_mutation(command.output, "host_profile", "copied", &target, &path)
            }
            (ProfileKind::Application, ProfileOperationKind::Delete) => {
                let id = application_profile_id(&command.ids[0])?;
                let path = catalog.application_path(&id);
                catalog
                    .delete_application(&id)
                    .map_err(profile_management_error)?;
                write_profile_mutation(command.output, "application_profile", "deleted", &id, &path)
            }
            (ProfileKind::Host, ProfileOperationKind::Delete) => {
                let id = host_profile_id(&command.ids[0])?;
                let path = catalog.host_path(&id);
                catalog.delete_host(&id).map_err(profile_management_error)?;
                write_profile_mutation(command.output, "host_profile", "deleted", &id, &path)
            }
            (ProfileKind::Host | ProfileKind::Application, ProfileOperationKind::Preview) => {
                unreachable!()
            }
        }
    })();
    result.map_or_else(|error| report_error(&error), |()| 0)
}

pub(super) async fn run_host_profile_preview(command: &ProfileCommand) -> u8 {
    let paths = match standard_paths() {
        Ok(paths) => paths,
        Err(error) => return report_error(&error),
    };
    let id = match host_profile_id(&command.ids[0]) {
        Ok(id) => id,
        Err(error) => return report_error(&error),
    };
    let document = match ProfileCatalog::new(paths.clone()).host(&id) {
        Ok(document) => document,
        Err(error) => return report_error(&profile_management_error(error)),
    };
    let coding = match standard_coding_tools() {
        Ok(coding) => coding,
        Err(error) => return report_error(&error),
    };
    let presets =
        match AgentPresetManager::open_standard_preview(paths.clone(), coding.is_some()).await {
            Ok(presets) => presets,
            Err(error) => return report_error(&error),
        };
    let preview = StandardComposition::new(paths, BTreeMap::new(), coding)
        .with_agent_presets(presets.catalog().clone())
        .preview_host(&document);
    let shutdown = presets.shutdown().await;
    let result = preview.and_then(|preview| {
        if !shutdown.is_clean() {
            return Err(RsiError::Boot(
                "Agent-preset preview shutdown was not clean".into(),
            ));
        }
        match command.output {
            ManagementOutput::Json => write_json(&serde_json::json!({
                "version": 1,
                "type": "host_profile_preview",
                "id": id.as_str(),
                "launch_key": preview.launch_key.as_str(),
                "source_digest": preview.profile.source_digest,
                "source_paths": preview.profile.source_paths,
                "leaves": preview.profile.leaves.iter().map(|leaf| serde_json::json!({
                    "instance_id": leaf.instance_id,
                    "plugin_id": leaf.plugin_id,
                })).collect::<Vec<_>>(),
            })),
            ManagementOutput::Text => write_text_line(&format!(
                "id: {}\nlaunch-key: {}\nsource-digest: {}\nleaves: {}",
                id,
                preview.launch_key,
                preview.profile.source_digest,
                preview.profile.leaves.len()
            )),
        }
    });
    result.map_or_else(|error| report_error(&error), |()| 0)
}

pub(super) fn application_profile_id(value: &str) -> rsi::Result<ApplicationProfileId> {
    ApplicationProfileId::new(value).map_err(profile_management_error)
}

pub(super) fn host_profile_id(value: &str) -> rsi::Result<HostProfileId> {
    HostProfileId::new(value).map_err(profile_management_error)
}

#[allow(clippy::needless_pass_by_value)] // Kept as a direct `map_err` adapter.
pub(super) fn profile_management_error(error: rsi::ProfileCatalogError) -> RsiError {
    RsiError::Boot(error.to_string())
}

pub(super) fn profile_source_name(source: ProfileSource) -> &'static str {
    match source {
        ProfileSource::Builtin => "builtin",
        ProfileSource::User => "user",
    }
}

pub(super) fn write_profile_rows(
    output: ManagementOutput,
    kind: &'static str,
    rows: Vec<(String, ProfileSource)>,
) -> rsi::Result<()> {
    match output {
        ManagementOutput::Json => write_json(&serde_json::json!({
            "version": 1,
            "type": kind,
            "profiles": rows.iter().map(|(id, source)| serde_json::json!({
                "id": id,
                "source": profile_source_name(*source),
            })).collect::<Vec<_>>(),
        })),
        ManagementOutput::Text => {
            let mut text = String::from("ID\tSOURCE");
            for (id, source) in rows {
                text.push('\n');
                text.push_str(&id);
                text.push('\t');
                text.push_str(profile_source_name(source));
            }
            write_text_line(&text)
        }
    }
}

pub(super) fn write_profile_document(
    output: ManagementOutput,
    kind: &'static str,
    id: &str,
    source: ProfileSource,
    path: Option<&std::path::Path>,
    contents: &str,
) -> rsi::Result<()> {
    match output {
        ManagementOutput::Json => write_json(&serde_json::json!({
            "version": 1,
            "type": kind,
            "id": id,
            "source": profile_source_name(source),
            "path": path,
            "contents": contents,
        })),
        ManagementOutput::Text => write_text_line(&format!(
            "id: {id}\nsource: {}\npath: {}\ncontents:\n{contents}",
            profile_source_name(source),
            path.map_or_else(|| "<builtin>".into(), |path| path.display().to_string())
        )),
    }
}

pub(super) fn write_profile_path(
    output: ManagementOutput,
    kind: &'static str,
    id: &str,
    source: ProfileSource,
    path: Option<&std::path::Path>,
) -> rsi::Result<()> {
    match output {
        ManagementOutput::Json => write_json(&serde_json::json!({
            "version": 1,
            "type": kind,
            "id": id,
            "source": profile_source_name(source),
            "path": path,
        })),
        ManagementOutput::Text => write_text_line(
            &path.map_or_else(|| "<builtin>".into(), |path| path.display().to_string()),
        ),
    }
}

pub(super) fn write_profile_mutation<I: std::fmt::Display>(
    output: ManagementOutput,
    kind: &'static str,
    action: &'static str,
    id: &I,
    path: &std::path::Path,
) -> rsi::Result<()> {
    match output {
        ManagementOutput::Json => write_json(&serde_json::json!({
            "version": 1,
            "type": "profile_mutation",
            "profile_type": kind,
            "action": action,
            "id": id.to_string(),
            "path": path,
        })),
        ManagementOutput::Text => write_text_line(&format!("{action} {id}\t{}", path.display())),
    }
}

pub(super) async fn run_agent_preset(command: AgentPresetCommand) -> u8 {
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

pub(super) async fn shutdown_agent_preset_manager(
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

pub(super) async fn execute_agent_preset(
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

pub(super) async fn list_agent_presets(
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

pub(super) async fn show_agent_preset(
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

pub(super) fn path_agent_preset(
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
pub(super) struct PresetOutput {
    id: String,
    metadata: MetadataOutput,
    source: &'static str,
    trust: &'static str,
    status: &'static str,
    reason: Option<String>,
    default: bool,
}

#[derive(Debug, Serialize)]
pub(super) struct MetadataOutput {
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
pub(super) struct ListOutput {
    version: u8,
    #[serde(rename = "type")]
    kind: &'static str,
    authorable: bool,
    presets: Vec<PresetOutput>,
}

#[derive(Debug, Serialize)]
pub(super) struct ShowOutput {
    version: u8,
    #[serde(rename = "type")]
    kind: &'static str,
    preset: PresetOutput,
    composition: Option<String>,
}

#[derive(Debug, Serialize)]
pub(super) struct PathOutput<'a> {
    version: u8,
    #[serde(rename = "type")]
    kind: &'static str,
    id: &'a str,
    path: &'a str,
}

#[derive(Debug, Serialize)]
pub(super) struct AgentStoreVerifyOutput<'a> {
    version: u8,
    #[serde(rename = "type")]
    kind: &'static str,
    status: &'static str,
    root: &'a std::path::Path,
}

#[derive(Debug, Serialize)]
pub(super) struct ActionOutput<'a> {
    version: u8,
    #[serde(rename = "type")]
    kind: &'static str,
    action: &'a str,
    id: &'a str,
}

pub(super) fn source_name(source: AgentPresetSource) -> &'static str {
    match source {
        AgentPresetSource::System => "system",
        AgentPresetSource::Configured => "configured",
        AgentPresetSource::User => "user",
    }
}

pub(super) fn trust_name(trust: AgentPresetTrust) -> &'static str {
    match trust {
        AgentPresetTrust::System => "system",
        AgentPresetTrust::User => "user",
    }
}

pub(super) fn write_roster_text(presets: &[PresetOutput]) -> rsi::Result<()> {
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

pub(super) fn write_show_text(preset: &PresetOutput, composition: Option<&str>) -> rsi::Result<()> {
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

pub(super) fn write_action(
    output: ManagementOutput,
    action: &str,
    id: &AgentPresetId,
) -> rsi::Result<()> {
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

pub(super) fn write_default(
    output: ManagementOutput,
    action: &str,
    id: &AgentPresetId,
) -> rsi::Result<()> {
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

pub(super) fn write_json(value: &impl Serialize) -> rsi::Result<()> {
    let stdout = std::io::stdout();
    let mut stdout = stdout.lock();
    serde_json::to_writer(&mut stdout, value)
        .map_err(|error| RsiError::Boot(format!("stdout JSON write failed: {error}")))?;
    stdout
        .write_all(b"\n")
        .and_then(|()| stdout.flush())
        .map_err(output_error)
}

pub(super) fn write_text_line(value: &str) -> rsi::Result<()> {
    let stdout = std::io::stdout();
    let mut stdout = stdout.lock();
    writeln!(stdout, "{value}")
        .and_then(|()| stdout.flush())
        .map_err(output_error)
}

#[allow(clippy::needless_pass_by_value)] // Exact `map_err` adapter for owned I/O failures.
pub(super) fn output_error(error: std::io::Error) -> RsiError {
    RsiError::Boot(format!("stdout write failed: {error}"))
}

#[allow(clippy::needless_pass_by_value)] // Exact `map_err` adapter for owned catalog failures.
pub(super) fn preset_management_error(error: PresetError) -> RsiError {
    RsiError::Boot(error.to_string())
}
