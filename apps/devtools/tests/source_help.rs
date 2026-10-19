//! Help must work before setup, without publication metadata or build tools.
use std::{fs, path::PathBuf, process::Command};

fn shells() -> Vec<(PathBuf, Vec<&'static str>, &'static str)> {
    #[cfg(windows)]
    {
        let powershell = PathBuf::from(std::env::var_os("SystemRoot").unwrap())
            .join("System32/WindowsPowerShell/v1.0/powershell.exe");
        let pwsh = Command::new("pwsh.exe")
            .args(["-NoProfile", "-Command", "[Environment]::ProcessPath"])
            .output()
            .expect("PowerShell 7 is required for source-entry tests");
        assert!(pwsh.status.success());
        let pwsh = PathBuf::from(String::from_utf8(pwsh.stdout).unwrap().trim());
        let git = Command::new("git").arg("--exec-path").output().unwrap();
        assert!(git.status.success());
        let git = PathBuf::from(String::from_utf8(git.stdout).unwrap().trim());
        let bash = git
            .ancestors()
            .map(|path| path.join("bin/bash.exe"))
            .find(|path| path.is_file())
            .expect("Git for Windows Bash is required");
        let flags = vec!["-NoProfile", "-ExecutionPolicy", "Bypass", "-File"];
        vec![
            (powershell, flags.clone(), "rsiversi.ps1"),
            (pwsh, flags, "rsiversi.ps1"),
            (bash, vec![], "rsiversi.sh"),
        ]
    }
    #[cfg(not(windows))]
    {
        vec![(PathBuf::from("/bin/bash"), vec![], "rsiversi.sh")]
    }
}

#[cfg(target_os = "linux")]
#[test]
fn bash_launch_preserves_workspace_arguments_and_status() {
    use std::os::unix::fs::PermissionsExt;
    let root = tempfile::tempdir().unwrap();
    let caller = root.path().join("caller space 中文");
    fs::create_dir(&caller).unwrap();
    fs::create_dir(root.path().join("scripts")).unwrap();
    fs::copy(
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../scripts/rsiversi.sh"),
        root.path().join("scripts/rsiversi.sh"),
    )
    .unwrap();
    for target in ["tui", "web", "desktop"] {
        let base = root.path().join("target/rsi-app").join(target);
        let bundle = base.join("generations/00000000000000000001/bundle");
        fs::create_dir_all(&bundle).unwrap();
        fs::write(base.join("current"), b"00000000000000000001\n").unwrap();
        fs::write(bundle.join("receipt.json"), b"{}").unwrap();
        fs::write(bundle.join(".rsi-generation.lock"), b"").unwrap();
        for name in ["rsi", "rsi-desktop"] {
            let binary = bundle.join(name);
            fs::write(
                &binary,
                "#!/bin/sh\npwd -P\nprintf '<%s>\\n' \"$@\"\nexit 37\n",
            )
            .unwrap();
            fs::set_permissions(binary, fs::Permissions::from_mode(0o755)).unwrap();
        }
        let output = Command::new("/bin/bash")
            .arg(root.path().join("scripts/rsiversi.sh"))
            .args([target, "", "two words", "中文", "relative/file"])
            .current_dir(&caller)
            .env_remove("BASH_ENV")
            .output()
            .unwrap();
        assert_eq!(
            output.status.code(),
            Some(37),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let stdout = String::from_utf8(output.stdout).unwrap();
        let lines: Vec<_> = stdout.lines().collect();
        assert_eq!(lines[0], caller.canonicalize().unwrap().to_str().unwrap());
        let mut expected = vec![];
        if target != "desktop" {
            expected.push(format!("<{target}>"));
        }
        expected.extend(["<>", "<two words>", "<中文>", "<relative/file>"].map(str::to_owned));
        assert_eq!(&lines[1..], expected);
    }
}

#[test]
fn source_help_precedes_prerequisites_and_has_no_side_effects() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().join("source space 中文");
    let scripts = root.join("scripts");
    fs::create_dir_all(&scripts).unwrap();
    let source = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../scripts");
    for name in ["rsiversi.ps1", "rsiversi.sh"] {
        fs::copy(source.join(name), scripts.join(name)).unwrap();
    }
    let mut cases = vec![vec![], vec!["--help"], vec!["-h"]];
    for operation in ["doctor", "setup", "build", "path"] {
        for flag in ["--help", "-h"] {
            cases.push(vec![operation, flag]);
        }
    }
    cases.extend([
        vec!["build", "--debug", "--help"],
        vec!["build", "-h", "--debug"],
        vec![
            "build",
            "desktop",
            "--with-ssh-helper",
            "--verbose",
            "--help",
        ],
        vec!["setup", "web", "--with-ssh-helper", "-h"],
        vec!["doctor", "unknown-target", "-h"],
        vec!["path", "desktop", "--help"],
    ]);
    for (shell, flags, script) in shells() {
        for arguments in &cases {
            let output = Command::new(&shell)
                .args(&flags)
                .arg(scripts.join(script))
                .args(arguments)
                .env("PATH", &scripts)
                .env_remove("BASH_ENV")
                .env_remove("WT_SESSION")
                .current_dir(temporary.path())
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{shell:?} {arguments:?}: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            assert!(output.stderr.is_empty(), "{shell:?}: {:?}", output.stderr);
            let text = String::from_utf8(output.stdout).unwrap();
            assert!(text.contains("Usage:") && text.contains(script), "{text}");
            if let Some(operation) = arguments
                .first()
                .filter(|argument| !argument.starts_with('-'))
            {
                assert!(text.contains(&format!("{script} {operation}")), "{text}");
            } else {
                for command in ["doctor", "setup", "build", "web", "tui", "desktop", "path"] {
                    assert!(text.contains(command), "{text}");
                }
                assert!(
                    text.contains("release") && text.contains("--debug"),
                    "{text}"
                );
            }
            assert_eq!(
                fs::read_dir(&root).unwrap().count(),
                1,
                "help wrote into the checkout"
            );
        }
        let output = Command::new(&shell)
            .args(&flags)
            .arg(scripts.join(script))
            .arg("unknown-command")
            .env("PATH", &scripts)
            .env_remove("BASH_ENV")
            .current_dir(temporary.path())
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(1));
        assert!(output.stdout.is_empty());
        let error = String::from_utf8_lossy(&output.stderr);
        assert!(
            error.contains("unknown-command") && error.contains("--help"),
            "{error}"
        );
    }
}

#[cfg(windows)]
#[test]
fn windows_output_paths_reach_prerequisites_consistently_across_shells() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().join("source space 中文");
    fs::create_dir_all(root.join("scripts")).unwrap();
    fs::create_dir_all(root.join("apps/web")).unwrap();
    let source = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    for name in [
        "scripts/rsiversi.ps1",
        "scripts/rsiversi.sh",
        "rust-toolchain.toml",
        "Cargo.lock",
        "apps/web/package.json",
    ] {
        fs::copy(source.join(name), root.join(name)).unwrap();
    }
    let powershell = PathBuf::from(std::env::var_os("SystemRoot").unwrap())
        .join("System32/WindowsPowerShell/v1.0");
    for (shell, flags, script) in shells() {
        let bash = script == "rsiversi.sh";
        let mut path = vec![powershell.clone()];
        if bash {
            path.push(shell.parent().unwrap().parent().unwrap().join("usr/bin"));
        }
        let mut cases = vec![
            ("C:/output space 中文", true),
            (r"C:\output space 中文", true),
            ("//server/share/output space 中文", true),
            (r"\\server\share\output space 中文", true),
            ("C:relative", false),
            ("relative/path", false),
            ("--unknown", false),
        ];
        if bash {
            cases.push(("/c/output space 中文", true));
        }
        for (argument, admitted) in cases {
            let mut command = Command::new(&shell);
            if bash {
                // MSYS parses native Windows argv differently from the CRT.
                // Enter these paths as quoted Bash source, as an interactive user does.
                let invocation = root.join("invoke.sh");
                let quote = |value: &str| format!("'{}'", value.replace('\'', "'\\''"));
                fs::write(
                    &invocation,
                    format!(
                        "exec bash {} build tui {} --debug\n",
                        quote(
                            &root
                                .join("scripts")
                                .join(script)
                                .to_string_lossy()
                                .replace('\\', "/")
                        ),
                        quote(argument),
                    ),
                )
                .unwrap();
                command.arg(invocation);
            } else {
                command
                    .args(&flags)
                    .arg(root.join("scripts").join(script))
                    .args(["build", "tui", argument, "--debug"]);
            }
            let output = command
                .env("PATH", std::env::join_paths(&path).unwrap())
                .env_remove("BASH_ENV")
                .env_remove("ProgramFiles(x86)")
                .current_dir(temporary.path())
                .output()
                .unwrap();
            assert_eq!(output.status.code(), Some(1));
            let diagnostic = String::from_utf8_lossy(&output.stderr);
            if admitted {
                assert!(
                    diagnostic.contains("Missing rustup.exe"),
                    "{shell:?} {argument:?}: {diagnostic}"
                );
                assert!(!diagnostic.contains("Invalid"), "{diagnostic}");
            } else {
                assert!(
                    diagnostic.contains("Invalid"),
                    "{shell:?} {argument:?}: {diagnostic}"
                );
                assert!(!diagnostic.contains("Missing rustup.exe"), "{diagnostic}");
            }
            assert!(!root.join("target").exists());
        }
    }
}

#[cfg(target_os = "linux")]
#[test]
fn bash_output_paths_use_native_absolute_grammar_before_prerequisites() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().join("source space 中文");
    let scripts = root.join("scripts");
    fs::create_dir_all(&scripts).unwrap();
    fs::copy(
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../scripts/rsiversi.sh"),
        scripts.join("rsiversi.sh"),
    )
    .unwrap();
    for name in ["dirname", "uname"] {
        std::os::unix::fs::symlink(PathBuf::from("/usr/bin").join(name), scripts.join(name))
            .unwrap();
    }
    for (path, admitted) in [
        ("/tmp/output space 中文", true),
        ("//server/share/out", true),
        ("C:/output", false),
        ("C:relative", false),
        ("relative/path", false),
    ] {
        let output = Command::new("/bin/bash")
            .arg(scripts.join("rsiversi.sh"))
            .args(["build", "tui", path, "--debug"])
            .env("PATH", &scripts)
            .env_remove("BASH_ENV")
            .current_dir(temporary.path())
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(1));
        let diagnostic = String::from_utf8_lossy(&output.stderr);
        assert_eq!(
            diagnostic.contains("Missing git;"),
            admitted,
            "{path}: {diagnostic}"
        );
        assert_eq!(
            diagnostic.contains("Invalid argument"),
            !admitted,
            "{path}: {diagnostic}"
        );
        assert!(!root.join("target").exists());
    }
}
