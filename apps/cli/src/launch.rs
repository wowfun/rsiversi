use super::{OsString, PathBuf, RsiError};
use rsi::{HomeOverrides, LaunchConfiguration};
use rsi_host::HostPaths;
use std::sync::OnceLock;

static STARTUP: OnceLock<Startup> = OnceLock::new();

#[derive(Clone, Debug, Default)]
pub(super) struct Options {
    pub homes: HomeOverrides,
    pub watch: bool,
    pub reset_config: bool,
    pub reset_state: bool,
}

#[derive(Debug)]
pub(super) struct Startup {
    pub paths: HostPaths,
    pub options: Options,
}

impl Startup {
    pub fn configuration(&self) -> rsi::Result<LaunchConfiguration> {
        LaunchConfiguration::load(self.paths.config(), self.options.homes.config.is_some())
    }

    pub fn append_child_options(&self, command: &mut std::process::Command) {
        // Preserve default-document semantics while freezing explicit roots.
        for (flag, selected, resolved) in [
            (
                "--config-home",
                &self.options.homes.config,
                self.paths.config(),
            ),
            (
                "--state-home",
                &self.options.homes.state,
                self.paths.state(),
            ),
            (
                "--cache-home",
                &self.options.homes.cache,
                self.paths.cache(),
            ),
        ] {
            if selected.is_some() {
                command.arg(flag).arg(resolved);
            }
        }
        if self.options.watch {
            command.arg("--watch");
        }
    }
}

pub(super) fn current() -> rsi::Result<&'static Startup> {
    STARTUP
        .get()
        .ok_or_else(|| RsiError::Boot("launch inputs were not resolved".into()))
}

pub(super) fn paths() -> rsi::Result<HostPaths> {
    STARTUP
        .get()
        .map_or_else(rsi::standard_paths, |startup| Ok(startup.paths.clone()))
}

pub(super) fn maintain() -> rsi::Result<()> {
    let startup = current()?;
    let selected = rsi::ResetSelection {
        config: startup.options.reset_config,
        state: startup.options.reset_state,
    };
    for receipt in rsi::maintain_product_homes(&startup.paths, Some(selected))? {
        eprintln!("Reset completed: {}", receipt.transaction);
        for backup in receipt.backups {
            eprintln!("Preserved backup: {}", backup.display());
        }
    }
    Ok(())
}

pub(super) fn validate_command(options: &Options, command: &super::Parse) -> rsi::Result<()> {
    if !(options.reset_config || options.reset_state) {
        return Ok(());
    }
    let refused = matches!(command, super::Parse::WebControl(_));
    #[cfg(any(target_os = "linux", windows))]
    let refused = refused
        || matches!(
            command,
            super::Parse::Host(super::HostCommand {
                operation: super::HostOperation::Stop
                    | super::HostOperation::Status
                    | super::HostOperation::Reload,
                ..
            })
        );
    if refused {
        return Err(RsiError::Boot(
            "reset requires an application, config command, or host start/serve/restart".into(),
        ));
    }
    Ok(())
}

pub(super) fn initialize(options: Options) -> rsi::Result<()> {
    let cwd = std::env::current_dir().map_err(|error| RsiError::Boot(error.to_string()))?;
    let paths = options.homes.resolve(&rsi::standard_paths()?, &cwd)?;
    STARTUP
        .set(Startup { paths, options })
        .map_err(|_| RsiError::Boot("launch inputs were already resolved".into()))
}

pub(super) fn parse(
    arguments: impl IntoIterator<Item = OsString>,
) -> rsi::Result<(Options, Vec<OsString>)> {
    let mut arguments = arguments.into_iter().peekable();
    let mut options = Options::default();
    while let Some(argument) = arguments.peek().and_then(|value| value.to_str()) {
        let path = match argument {
            "--config-home" => Some(&mut options.homes.config),
            "--state-home" => Some(&mut options.homes.state),
            "--cache-home" => Some(&mut options.homes.cache),
            _ => None,
        };
        if let Some(path) = path {
            let option = arguments.next().expect("peeked argument");
            if path.is_some() {
                return Err(super::cli::usage(format!(
                    "duplicate {}",
                    option.to_string_lossy()
                )));
            }
            let value = arguments
                .next()
                .filter(|value| !value.is_empty())
                .ok_or_else(|| {
                    super::cli::usage(format!("{} requires DIR", option.to_string_lossy()))
                })?;
            if value.to_str().is_some_and(|value| value.starts_with("--")) {
                return Err(super::cli::usage(format!(
                    "{} requires DIR",
                    option.to_string_lossy()
                )));
            }
            *path = Some(PathBuf::from(value));
            continue;
        }
        let flag = match argument {
            "--watch" => &mut options.watch,
            "--reset-config" => &mut options.reset_config,
            "--reset-state" => &mut options.reset_state,
            _ => break,
        };
        let option = arguments.next().expect("peeked argument");
        if *flag {
            return Err(super::cli::usage(format!(
                "duplicate {}",
                option.to_string_lossy()
            )));
        }
        *flag = true;
    }
    Ok((options, arguments.collect()))
}

pub(super) fn run_config(arguments: &[OsString]) -> u8 {
    let result = (|| -> rsi::Result<()> {
        let startup = current()?;
        let command = arguments
            .first()
            .and_then(|argument| argument.to_str())
            .unwrap_or("show");
        if arguments.len() > 1 {
            return Err(super::cli::usage(
                "config accepts one operation: init, show or validate",
            ));
        }
        if !matches!(command, "init" | "show" | "validate") {
            return Err(super::cli::usage(
                "unknown config operation; use init, show or validate",
            ));
        }
        maintain()?;
        match command {
            "init" => {
                let path = if startup.options.reset_config {
                    startup.paths.config().join(rsi::LAUNCH_FILE)
                } else {
                    LaunchConfiguration::initialize(startup.paths.config())?
                };
                println!("{}", path.display());
            }
            "show" => {
                let document = startup.configuration()?;
                let value = serde_json::json!({
                    "config_home": startup.paths.config(),
                    "state_home": startup.paths.state(),
                    "cache_home": startup.paths.cache(),
                    "configuration": document,
                });
                println!(
                    "{}",
                    serde_json::to_string_pretty(&value)
                        .map_err(|error| RsiError::Boot(error.to_string()))?
                );
            }
            "validate" => {
                startup.configuration()?;
                println!(
                    "valid\t{}",
                    startup.paths.config().join(rsi::LAUNCH_FILE).display()
                );
            }
            _ => {
                return Err(super::cli::usage(
                    "unknown config operation; use init, show or validate",
                ));
            }
        }
        Ok(())
    })();
    result.map_or_else(|error| super::report_error(&error), |()| 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn options(args: &[&str]) -> rsi::Result<(Options, Vec<OsString>)> {
        parse(args.iter().map(OsString::from))
    }

    #[test]
    fn global_options_are_prefix_only_and_preserve_application_arguments() {
        let (parsed, rest) = options(&[
            "--state-home",
            "./state",
            "--watch",
            "headless",
            "--",
            "--reset-config",
        ])
        .unwrap();
        assert_eq!(parsed.homes.state, Some("./state".into()));
        assert!(parsed.watch);
        assert!(!parsed.reset_config);
        assert_eq!(rest, ["headless", "--", "--reset-config"]);
        let (parsed, rest) = options(&["tui", "--state-home", "./other"]).unwrap();
        assert_eq!(parsed.homes, HomeOverrides::default());
        assert_eq!(rest, ["tui", "--state-home", "./other"]);
    }

    #[test]
    fn independent_reset_flags_and_duplicate_rejection() {
        let (parsed, rest) = options(&["--reset-config", "--reset-state"]).unwrap();
        assert!(parsed.reset_config && parsed.reset_state && rest.is_empty());
        for args in [
            vec!["--watch", "--watch"],
            vec!["--config-home"],
            vec!["--config-home", "--watch"],
            vec!["--state-home", "a", "--state-home", "b"],
        ] {
            assert!(options(&args).is_err());
        }
    }
}
