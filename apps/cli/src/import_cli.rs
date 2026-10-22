use super::cli::IMPORT_HELP as HELP;
use super::{OsString, RsiError};

pub(super) fn run(arguments: &[OsString]) -> u8 {
    let result = (|| -> rsi::Result<rsi::ImportReceipt> {
        let startup = super::launch::current()?;
        if startup.options.reset_config || startup.options.reset_state || startup.options.watch {
            return Err(error("import cannot be combined with reset or watch"));
        }
        if matches!(arguments, [command] if command == "recover") {
            return rsi::recover_product_import(&startup.paths);
        }
        let mut config = None;
        let mut state = None;
        let mut dry_run = false;
        let mut args = arguments.iter();
        while let Some(arg) = args.next() {
            let slot = match arg.to_str() {
                Some("--config-home") => &mut config,
                Some("--state-home") => &mut state,
                Some("--dry-run") if !dry_run => {
                    dry_run = true;
                    continue;
                }
                _ => return Err(error(HELP)),
            };
            if slot.is_some() {
                return Err(error("duplicate import home"));
            }
            let path = args
                .next()
                .filter(|path| !path.is_empty() && !path.to_string_lossy().starts_with("--"))
                .ok_or_else(|| error("source home requires a path"))?;
            *slot = Some(rsi::normalize_launch_path(
                std::path::Path::new(path),
                &std::env::current_dir().map_err(error)?,
            )?);
        }
        let config = config.ok_or_else(|| error(HELP))?;
        let state = state.ok_or_else(|| error(HELP))?;
        let source =
            rsi_host::HostPaths::new(config, state.clone(), state.join("unused-import-cache"))
                .map_err(error)?;
        super::launch::maintain()?;
        rsi::import_product_homes(&startup.paths, &source, dry_run)
    })();
    match result {
        Ok(receipt) => {
            println!(
                "{}",
                serde_json::to_string_pretty(&receipt).expect("serializable receipt")
            );
            0
        }
        Err(error) => super::report_error(&error),
    }
}
fn error(value: impl std::fmt::Display) -> RsiError {
    RsiError::Boot(format!("import: {value}"))
}
