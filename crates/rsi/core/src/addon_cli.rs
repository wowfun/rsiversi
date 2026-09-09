#[cfg(unix)]
use super::{ManagementOutput, Parse, RsiError, report_error};
#[cfg(unix)]
use std::{ffi::OsString, path::PathBuf};

pub(super) const HELP: &str = "Usage:\n  rsi addon list [--root ABSOLUTE] [--output text|json]\n  rsi addon install MANIFEST [--root ABSOLUTE] [--output text|json]\n  rsi addon <enable|disable|uninstall> ID [--root ABSOLUTE] [--output text|json]\nInstallation never executes or enables artifacts. Uninstall requires disabling first.\nSource receipts are separate from running Host staging; inspect with --profile inspector native.\n";
#[cfg(unix)]
#[derive(Debug)]
pub(super) struct Command {
    operation: Operation,
    root: Option<PathBuf>,
    output: ManagementOutput,
}
#[cfg(unix)]
#[derive(Debug)]
enum Operation {
    List,
    Install(PathBuf),
    Enable(String),
    Disable(String),
    Uninstall(String),
}
#[cfg(unix)]
impl Operation {
    const fn name(&self) -> &'static str {
        match self {
            Self::List => "list",
            Self::Install(_) => "install",
            Self::Enable(_) => "enable",
            Self::Disable(_) => "disable",
            Self::Uninstall(_) => "uninstall",
        }
    }
}
#[cfg(unix)]
pub(super) fn parse(arguments: impl Iterator<Item = OsString>) -> rsi::Result<Parse> {
    let mut positional = Vec::new();
    let mut root = None;
    let mut output = None;
    let mut arguments = arguments.peekable();
    let invalid = || RsiError::Boot(HELP.into());
    while let Some(argument) = arguments.next() {
        match argument.to_str() {
            Some("--help" | "-h") => return Ok(Parse::Help(HELP)),
            Some("--root") if root.is_none() => {
                let path = PathBuf::from(arguments.next().ok_or_else(invalid)?);
                if !path.is_absolute() {
                    return Err(invalid());
                }
                root = Some(path);
            }
            Some("--output") if output.is_none() => {
                output = Some(
                    match arguments
                        .next()
                        .as_deref()
                        .and_then(std::ffi::OsStr::to_str)
                    {
                        Some("text") => ManagementOutput::Text,
                        Some("json") => ManagementOutput::Json,
                        _ => return Err(invalid()),
                    },
                );
            }
            Some(value) if value.starts_with('-') => return Err(invalid()),
            _ if positional.len() < 2 => positional.push(argument),
            _ => return Err(invalid()),
        }
    }
    let operation = match positional.as_slice() {
        [kind] if kind == "list" => Operation::List,
        [kind, path] if kind == "install" => Operation::Install(PathBuf::from(path)),
        [kind, id] => {
            let id = id.to_str().ok_or_else(invalid)?.to_owned();
            match kind.to_str() {
                Some("enable") => Operation::Enable(id),
                Some("disable") => Operation::Disable(id),
                Some("uninstall") => Operation::Uninstall(id),
                _ => return Err(invalid()),
            }
        }
        _ => return Err(invalid()),
    };
    Ok(Parse::Addon(Command {
        operation,
        root,
        output: output.unwrap_or(ManagementOutput::Text),
    }))
}

#[cfg(unix)]
pub(super) async fn run(command: Command) -> u8 {
    let result = tokio::task::spawn_blocking(move || execute(command)).await;
    match result {
        Ok(Ok(())) => 0,
        Ok(Err(error)) => report_error(&error),
        Err(_) => report_error(&RsiError::Boot("native source worker failed".into())),
    }
}
#[cfg(unix)]
fn execute(command: Command) -> rsi::Result<()> {
    let root = if let Some(root) = command.root {
        root
    } else {
        super::standard_paths()?.config().join("native-addons")
    };
    let root = rsi_files_native_fs::resolve_absolute_root_alias(&root, true).map_err(boot)?;
    let store = rsi::NativeAddonStore::open(root).map_err(boot)?;
    let operation = command.operation.name();
    if let Operation::List = &command.operation {
        let value = store.snapshot().map_err(boot)?;
        return match command.output {
            ManagementOutput::Json => super::management::write_json(
                &serde_json::json!({"version": 1, "kind": "native_addon_list", "revision": value.revision.to_string(), "installed": value.installed, "enabled": value.enabled}),
            ),
            ManagementOutput::Text => {
                rsi_terminal::write_text_line(&format!("revision\t{}", value.revision))?;
                for (kind, rows) in [("installed", value.installed), ("enabled", value.enabled)] {
                    for row in rows {
                        rsi_terminal::write_text_line(&format!(
                            "{kind}\t{}\t{}\t{}\t{}",
                            row.id(),
                            row.plugin(),
                            row.target(),
                            row.artifact_sha256()
                        ))?;
                    }
                }
                Ok(())
            }
        };
    }
    let receipt = match command.operation {
        Operation::Install(path) => store.install(&std::path::absolute(path).map_err(boot)?),
        Operation::Enable(id) => store.enable(&id),
        Operation::Disable(id) => store.disable(&id),
        Operation::Uninstall(id) => store.uninstall(&id),
        Operation::List => unreachable!("handled list"),
    }
    .map_err(boot)?;
    match command.output {
        ManagementOutput::Json => super::management::write_json(
            &serde_json::json!({"version": 1, "kind": format!("native_addon_{operation}"), "revision": receipt.revision.to_string(), "changed": receipt.changed, "directory_synced": receipt.directory_synced, "record": receipt.record}),
        ),
        ManagementOutput::Text => rsi_terminal::write_text_line(&format!(
            "{operation}\trevision={}\tchanged={}\tdirectory_synced={}",
            receipt.revision,
            receipt.changed,
            receipt
                .directory_synced
                .map_or("not_written", |synced| if synced {
                    "true"
                } else {
                    "false"
                })
        )),
    }
}
#[cfg(unix)]
fn boot(error: impl std::fmt::Display) -> RsiError {
    RsiError::Boot(error.to_string())
}
