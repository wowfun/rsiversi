/// Command language and stable Tool/Jobs identity for one native shell.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ShellKind {
    /// Bash command syntax.
    Bash,
    /// PowerShell 5 or 7 command syntax.
    PowerShell,
}

impl ShellKind {
    /// Model-facing tool name and target program selector.
    pub const fn name(self) -> &'static str {
        match self {
            Self::Bash => "bash",
            Self::PowerShell => "pwsh",
        }
    }
    #[cfg(any(target_os = "linux", windows))]
    pub(crate) const fn producer(self) -> &'static str {
        match self {
            Self::Bash => "rsi.coding.bash",
            Self::PowerShell => "rsi.coding.pwsh",
        }
    }
    #[cfg(any(target_os = "linux", windows))]
    pub(crate) const fn description(self) -> &'static str {
        match self {
            Self::Bash => {
                "Run an exact Bash command. Foreground commands wait for the complete process group; use run_in_background for long-lived work. Nonzero exits, signals, and command timeout are normal outcomes."
            }
            Self::PowerShell => {
                "Run a PowerShell command in a fresh native process. Use PowerShell syntax, native paths and $env:NAME for environment variables. Do not assign to automatic variables such as $HOME; names are case-insensitive. Foreground commands wait for descendant settlement; use run_in_background for long-lived work. Nonzero exits and command timeout are normal outcomes."
            }
        }
    }
    #[cfg(any(target_os = "linux", windows))]
    pub(crate) fn arguments(self, command: String) -> Vec<String> {
        match self {
            Self::Bash => vec!["--noprofile".into(), "--norc".into(), "-c".into(), command],
            Self::PowerShell => vec![
                "-NoLogo".into(),
                "-NoProfile".into(),
                "-NonInteractive".into(),
                "-Command".into(),
                format!(
                    "[Console]::OutputEncoding = [System.Text.UTF8Encoding]::new($false); $OutputEncoding = [System.Text.UTF8Encoding]::new($false); {command}"
                ),
            ],
        }
    }
}
