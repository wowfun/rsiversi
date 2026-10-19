use super::{Path, PathBuf, Result};

pub(super) const USAGE: &str = "usage: rsi-app-tools dist tui|web|desktop [ABSOLUTE_OUTPUT] [--debug] [--with-ssh-helper] [--verbose] | dist addon-workbench ABSOLUTE_NEW_REPORT";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Kind {
    Tui,
    Web,
    Desktop,
    AddonWorkbench,
}

impl Kind {
    pub(super) const APPLICATIONS: [Self; 3] = [Self::Tui, Self::Web, Self::Desktop];
    pub(super) fn parse(value: &str) -> Result<Self> {
        match value {
            "tui" => Ok(Self::Tui),
            "web" => Ok(Self::Web),
            "desktop" => Ok(Self::Desktop),
            "addon-workbench" => Ok(Self::AddonWorkbench),
            _ => Err(USAGE.into()),
        }
    }
    pub(super) const fn name(self) -> &'static str {
        match self {
            Self::Tui => "tui",
            Self::Web => "web",
            Self::Desktop => "desktop",
            Self::AddonWorkbench => "addon-workbench",
        }
    }
    pub(super) const fn web(self) -> bool {
        matches!(self, Self::Web | Self::Desktop)
    }
}

pub(super) struct Options {
    pub(super) kind: Kind,
    pub(super) output: Option<PathBuf>,
    pub(super) debug: bool,
    pub(super) with_ssh_helper: bool,
    pub(super) verbose: bool,
}
impl Options {
    pub(super) fn parse(args: &[String]) -> Result<Self> {
        let kind = Kind::parse(args.first().ok_or(USAGE)?)?;
        let mut value = Self {
            kind,
            output: None,
            debug: false,
            with_ssh_helper: false,
            verbose: false,
        };
        for arg in &args[1..] {
            match arg.as_str() {
                "--debug" if !value.debug => value.debug = true,
                "--with-ssh-helper" if !value.with_ssh_helper => value.with_ssh_helper = true,
                "--verbose" if !value.verbose => value.verbose = true,
                _ if value.output.is_none() && Path::new(arg).is_absolute() => {
                    value.output = Some(arg.into());
                }
                _ if value.output.is_none() && !arg.starts_with('-') => {
                    return Err(
                        format!("output must be an absolute directory: {arg}; {USAGE}").into(),
                    );
                }
                _ => {
                    return Err(format!(
                        "unknown or duplicate distribution argument: {arg}; {USAGE}"
                    )
                    .into());
                }
            }
        }
        if kind == Kind::AddonWorkbench
            && (value.output.is_none() || value.debug || value.with_ssh_helper || value.verbose)
        {
            return Err("addon-workbench requires one new absolute report directory".into());
        }
        if cfg!(windows) && value.with_ssh_helper {
            return Err(
                "SSH helper publication requires Linux; omit --with-ssh-helper on Windows".into(),
            );
        }
        Ok(value)
    }
    pub(super) const fn helper(&self) -> bool {
        !cfg!(windows) && (self.kind.web() || self.with_ssh_helper)
    }
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn targets_select_stages_without_changing_native_features() {
        for target in ["tui", "web", "desktop"] {
            let options = Options::parse(&[target.into()]).unwrap();
            assert!(!options.debug && !options.verbose);
            assert_eq!(options.kind.web(), target != "tui");
            assert_eq!(options.helper(), !cfg!(windows) && target != "tui");
            let helper = Options::parse(&[target.into(), "--with-ssh-helper".into()]);
            if cfg!(windows) {
                assert!(helper.is_err());
            } else {
                assert!(helper.unwrap().helper());
            }
        }
    }

    #[test]
    fn duplicate_or_unknown_flags_never_become_output_paths() {
        for args in [
            vec!["tui", "--debug", "--debug"],
            vec!["web", "--verbose", "--verbose"],
            vec!["desktop", "--unknown"],
            vec!["tui", "relative-output"],
        ] {
            assert!(
                Options::parse(&args.into_iter().map(str::to_owned).collect::<Vec<_>>()).is_err()
            );
        }
    }
}
