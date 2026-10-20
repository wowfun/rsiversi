//! Native process boundaries, without invoking a build from a launch command.
#![cfg(windows)]
use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Output},
};

fn powershell() -> PathBuf {
    Path::new(&std::env::var_os("SystemRoot").expect("Windows bootstrap tests require SystemRoot"))
        .join("System32/WindowsPowerShell/v1.0/powershell.exe")
}

fn run(command: &mut Command) -> Output {
    command.output().unwrap_or_else(|error| {
        panic!("Cannot run bootstrap test tool {command:?}: {error}. Required tools: inbox Windows PowerShell, PowerShell 7 (pwsh.exe), Git for Windows and Rust on PATH; see apps/devtools/README.md")
    })
}

fn hex(value: &str) -> String {
    use std::fmt::Write as _;
    value
        .as_bytes()
        .iter()
        .fold(String::new(), |mut text, byte| {
            write!(text, "{byte:02x}").unwrap();
            text
        })
}

#[test]
fn development_bootstrap_filters_build_secrets_but_preserves_runtime_and_uses_private_runner() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().join("source space 中文");
    let caller = temporary.path().join("caller space 中文");
    fs::create_dir_all(root.join("scripts")).unwrap();
    fs::create_dir_all(root.join("apps/devtools")).unwrap();
    fs::create_dir(&caller).unwrap();
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    for name in [
        "scripts/rsiversi.ps1",
        "scripts/rsiversi.sh",
        "apps/devtools/build-environment.txt",
    ] {
        fs::copy(source.join(name), root.join(name)).unwrap();
    }
    let helper = temporary.path().join("helper.rs");
    fs::write(&helper, r#"
use std::{env,fs,path::PathBuf};
fn main() {
    let exe=env::current_exe().unwrap();
    if exe.file_stem().unwrap()=="cargo" {
        assert!(env::var_os("DEEPSEEK_API_KEY").is_none());
        assert!(env::var_os("CARGO_PKG_NAME").is_none());
        let args:Vec<_>=env::args_os().collect();
        let target=PathBuf::from(&args[args.iter().position(|arg|arg=="--target-dir").unwrap()+1]);
        let policy=fs::read_to_string(target.parent().unwrap().join("apps/devtools/build-environment.txt")).unwrap();
        for (key,suffix) in policy.lines().filter_map(|line|line.split_once('=')) {
            assert_eq!(PathBuf::from(env::var_os(key).unwrap()),target.join("rsi-app").join(suffix),"{key}");
        }
        fs::create_dir_all(target.join("debug")).unwrap();
        fs::copy(exe,target.join("debug/rsi-app-tools.exe")).unwrap();
        fs::write(target.join("build-filtered"),b"provider key and Cargo runtime context absent").unwrap();
    } else {
        assert_eq!(env::var("DEEPSEEK_API_KEY").unwrap(),"synthetic-not-a-provider-key");
        assert!(!exe.parent().unwrap().ends_with("debug"));
        let root=env::current_dir().unwrap();
        fs::write(root.join("runtime-caller"),env::var_os("RSI_DEV_CALLER_CWD").unwrap().to_string_lossy().as_bytes()).unwrap();
        fs::write(root.join("private-runner"),exe.to_string_lossy().as_bytes()).unwrap();
    }
}
"#).unwrap();
    let fake_cargo = temporary.path().join("cargo.exe");
    assert!(
        run(Command::new("rustc").arg(helper).arg("-o").arg(&fake_cargo))
            .status
            .success()
    );
    let search = std::env::join_paths(
        std::iter::once(temporary.path().to_path_buf())
            .chain(std::env::split_paths(&std::env::var_os("PATH").unwrap())),
    )
    .unwrap();
    let harness = temporary.path().join("debugger-harness.ps1");
    fs::write(&harness, r"
param([string]$Source)
$originalDebugMode = [runspace]::DefaultRunspace.Debugger.DebugMode
function Get-Command {
    param($Name, $CommandType, $ErrorAction)
    if ([runspace]::DefaultRunspace.Debugger.DebugMode -ne [System.Management.Automation.DebugModes]::None) {
        throw 'Native work must own console break without entering the script debugger'
    }
    Microsoft.PowerShell.Core\Get-Command -Name $Name -CommandType $CommandType -ErrorAction $ErrorAction
}
. $Source dev web --no-open
if ([runspace]::DefaultRunspace.Debugger.DebugMode -ne $originalDebugMode) { throw 'Caller debugger mode was not restored' }
").unwrap();
    for shell in [powershell(), PathBuf::from("pwsh.exe")] {
        let output = run(Command::new(shell)
            .args(["-NoProfile", "-ExecutionPolicy", "Bypass", "-File"])
            .arg(&harness)
            .arg(root.join("scripts/rsiversi.ps1"))
            .current_dir(&caller)
            .env("PATH", &search)
            .env_remove("CARGO_MANIFEST_DIR")
            .env_remove("CARGO_PKG_NAME")
            .env_remove("RSI_DEV_CALLER_CWD")
            .env("DEEPSEEK_API_KEY", "synthetic-not-a-provider-key"));
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            fs::read_to_string(root.join("runtime-caller")).unwrap(),
            caller.to_str().unwrap()
        );
        assert!(root.join("target/build-filtered").exists());
        assert!(!Path::new(&fs::read_to_string(root.join("private-runner")).unwrap()).exists());
        // The shared executable is unlocked after the private runner exits.
        fs::copy(&fake_cargo, root.join("target/debug/rsi-app-tools.exe")).unwrap();
    }
}

#[test]
fn pinned_toolchain_diagnosis_matches_whole_version_lines() {
    let root = tempfile::tempdir().unwrap();
    fs::create_dir(root.path().join("scripts")).unwrap();
    fs::copy(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../scripts/rsiversi.ps1"),
        root.path().join("scripts/rsiversi.ps1"),
    )
    .unwrap();
    fs::write(
        root.path().join("rust-toolchain.toml"),
        "[toolchain]\nchannel = \"1.97.0\"\n",
    )
    .unwrap();
    let source = root.path().join("rustup.rs");
    fs::write(
        &source,
        "fn main(){println!(\"{}\",std::env::var(\"RSI_TEST_TOOLCHAIN\").unwrap())}",
    )
    .unwrap();
    assert!(
        run(Command::new("rustc")
            .arg(source)
            .arg("-o")
            .arg(root.path().join("rustup.exe")))
        .status
        .success()
    );
    for shell in [powershell(), PathBuf::from("pwsh.exe")] {
        for (version, matches) in [
            ("11.97.0-x86_64-pc-windows-msvc", false),
            ("1.97.01-x86_64-pc-windows-msvc", false),
            ("1.97.0-x86_64-pc-windows-msvc (default)", true),
        ] {
            let output = run(Command::new(&shell)
                .args(["-NoProfile", "-File"])
                .arg(root.path().join("scripts/rsiversi.ps1"))
                .args(["doctor", "tui"])
                .env("PATH", root.path())
                .env("RSI_TEST_TOOLCHAIN", version)
                .env_remove("ProgramFiles(x86)"));
            let error = String::from_utf8_lossy(&output.stderr);
            assert_eq!(
                !error.contains("Pinned Rust toolchain missing"),
                matches,
                "{version}: {error}"
            );
            if !matches {
                assert!(error.contains("setup tui"));
            }
        }
    }
}

#[test]
fn doctor_reports_missing_tools_before_rust_or_node_can_run() {
    let root = tempfile::tempdir().unwrap();
    fs::create_dir(root.path().join("scripts")).unwrap();
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    for name in ["scripts/rsiversi.ps1", "Cargo.lock", "rust-toolchain.toml"] {
        fs::copy(source.join(name), root.path().join(name)).unwrap();
    }
    fs::create_dir_all(root.path().join("apps/web")).unwrap();
    fs::copy(
        source.join("apps/web/package.json"),
        root.path().join("apps/web/package.json"),
    )
    .unwrap();
    let output = run(Command::new(powershell())
        .args(["-NoProfile", "-ExecutionPolicy", "Bypass", "-File"])
        .arg(root.path().join("scripts/rsiversi.ps1"))
        .args(["doctor", "web"])
        .env("PATH", root.path())
        .env_remove("ProgramFiles(x86)"));
    assert_eq!(output.status.code(), Some(1));
    let diagnostic = String::from_utf8_lossy(&output.stderr);
    for expected in [
        "Missing git.exe",
        "Missing rustup.exe",
        "Missing node.exe",
        "Missing npm.cmd",
        "MSVC x64",
    ] {
        assert!(diagnostic.contains(expected), "{diagnostic}");
    }
    assert!(!root.path().join("target").exists());
}

#[test]
fn doctor_accumulates_non_checkout_and_missing_tools_in_both_powershells() {
    let root = tempfile::tempdir().unwrap();
    fs::create_dir(root.path().join("scripts")).unwrap();
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    fs::copy(
        source.join("scripts/rsiversi.ps1"),
        root.path().join("scripts/rsiversi.ps1"),
    )
    .unwrap();
    for name in ["rust-toolchain.toml", "Cargo.lock", "apps/web/package.json"] {
        let target = root.path().join(name);
        fs::create_dir_all(target.parent().unwrap()).unwrap();
        fs::copy(source.join(name), target).unwrap();
    }
    // A native program emitting stderr reproduces PowerShell 5.1's terminating
    // NativeCommandError without relying on the host's Git installation layout.
    let fixture = root.path().join("git.rs");
    fs::write(
        &fixture,
        "fn main(){eprintln!(\"fatal: not a git repository\");std::process::exit(128)}",
    )
    .unwrap();
    assert!(
        run(Command::new("rustc")
            .arg(&fixture)
            .arg("-o")
            .arg(root.path().join("git.exe")))
        .status
        .success()
    );
    for shell in [powershell(), PathBuf::from("pwsh.exe")] {
        let output = run(Command::new(shell)
            .args(["-NoProfile", "-ExecutionPolicy", "Bypass", "-File"])
            .arg(root.path().join("scripts/rsiversi.ps1"))
            .args(["doctor", "web"])
            .env("PATH", root.path())
            .env_remove("ProgramFiles(x86)"));
        assert_eq!(output.status.code(), Some(1));
        let diagnostic = String::from_utf8_lossy(&output.stderr);
        for expected in [
            "source ZIP archives are unsupported",
            "Missing rustup.exe",
            "Missing node.exe",
            "Missing npm.cmd",
            "MSVC x64",
        ] {
            assert!(diagnostic.contains(expected), "{diagnostic}");
        }
        assert!(!root.path().join("target").exists());
    }
}

#[test]
#[allow(clippy::too_many_lines)] // All native shell boundaries share one isolated published fixture.
fn windows_launchers_preserve_unicode_empty_quotes_and_exit_without_build_tools() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary
        .path()
        .join("source space 中文")
        .join("long-checkout-component-".repeat(4));
    fs::create_dir_all(root.join("scripts")).unwrap();
    assert!(root.as_os_str().len() > 100);
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    for name in ["scripts/rsiversi.ps1", "scripts/rsiversi.sh"] {
        fs::copy(source.join(name), root.join(name)).unwrap();
    }
    let bundle = root.join("target/rsi-app/web/generations/00000000000000000001/bundle");
    fs::create_dir_all(&bundle).unwrap();
    fs::write(
        root.join("target/rsi-app/web/current"),
        b"00000000000000000001\n",
    )
    .unwrap();
    fs::write(bundle.join("receipt.json"), b"{}").unwrap();
    fs::write(bundle.join(".rsi-generation.lock"), b"").unwrap();
    let fixture = root.join("probe.rs");
    fs::write(&fixture, "fn main(){if let Some(path)=std::env::var_os(\"RSI_SOURCE_PROBE_CWD\"){std::fs::write(path,std::env::current_dir().unwrap().to_str().unwrap()).unwrap();}if let Some(path)=std::env::var_os(\"RSI_SOURCE_PROBE_PATH\"){std::fs::write(path,std::env::current_exe().unwrap().to_str().unwrap()).unwrap();}for arg in std::env::args().skip(1){println!(\"{}\",arg.as_bytes().iter().map(|b|format!(\"{b:02x}\")).collect::<String>());}std::process::exit(37)}").unwrap();
    assert!(
        run(Command::new("rustc")
            .args(["--crate-name", "source_entry_probe"])
            .arg(&fixture)
            .arg("-o")
            .arg(bundle.join("rsi.exe")))
        .status
        .success()
    );
    for target in ["tui", "desktop"] {
        let target_root = root.join("target/rsi-app").join(target);
        let selected = target_root.join("generations/00000000000000000001/bundle");
        fs::create_dir_all(&selected).unwrap();
        fs::write(target_root.join("current"), b"00000000000000000001\n").unwrap();
        for name in ["rsi.exe", "receipt.json", ".rsi-generation.lock"] {
            fs::copy(bundle.join(name), selected.join(name)).unwrap();
        }
        if target == "desktop" {
            fs::copy(bundle.join("rsi.exe"), selected.join("rsi-desktop.exe")).unwrap();
        }
    }
    let caller = temporary.path().join("caller space 中文");
    fs::create_dir(&caller).unwrap();
    let cwd_report = temporary.path().join("cwd.txt");
    let arguments = [
        "web",
        "",
        "two words",
        "中文路径",
        "quote\"inside",
        "C:\\trailing\\",
        "--config={\"key\":\"value with space\"}",
    ];
    let expected = arguments.iter().map(|arg| hex(arg)).collect::<Vec<_>>();
    let invocation = format!(
        "& '{}' {}\nexit $LASTEXITCODE\n",
        root.join("scripts/rsiversi.ps1").display(),
        arguments
            .iter()
            .map(|arg| format!("'{}'", arg.replace('\'', "''")))
            .collect::<Vec<_>>()
            .join(" ")
    );
    let script = root.join("invoke.ps1");
    let mut bytes = vec![0xef, 0xbb, 0xbf];
    bytes.extend_from_slice(invocation.as_bytes());
    fs::write(&script, bytes).unwrap();
    for shell in [powershell(), PathBuf::from("pwsh.exe")] {
        let output = run(Command::new(&shell)
            .args(["-NoProfile", "-ExecutionPolicy", "Bypass", "-File"])
            .arg(&script)
            .current_dir(&caller)
            .env("RSI_SOURCE_PROBE_CWD", &cwd_report));
        assert_eq!(
            output.status.code(),
            Some(37),
            "{shell:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            String::from_utf8(output.stdout)
                .unwrap()
                .lines()
                .collect::<Vec<_>>(),
            expected,
            "{shell:?}"
        );
        assert_eq!(
            fs::read_to_string(&cwd_report).unwrap(),
            caller.to_str().unwrap()
        );
        let path_script = root.join("path.ps1");
        let mut bytes = vec![0xef, 0xbb, 0xbf];
        bytes.extend_from_slice(format!(
            "$p = & '{}' path web\n[Console]::WriteLine([BitConverter]::ToString([Text.Encoding]::UTF8.GetBytes($p)).Replace('-', '').ToLowerInvariant())\n",
            root.join("scripts/rsiversi.ps1").display(),
        ).as_bytes());
        fs::write(&path_script, bytes).unwrap();
        let output = run(Command::new(&shell)
            .args(["-NoProfile", "-ExecutionPolicy", "Bypass", "-File"])
            .arg(path_script));
        assert!(output.status.success(), "{shell:?}");
        let encoded = String::from_utf8(output.stdout).unwrap();
        let bytes = encoded
            .trim()
            .as_bytes()
            .chunks_exact(2)
            .map(|pair| u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap())
            .collect::<Vec<_>>();
        assert_eq!(encoded.trim().len(), bytes.len() * 2);
        let actual = PathBuf::from(String::from_utf8(bytes).unwrap());
        assert_eq!(
            actual.canonicalize().unwrap(),
            bundle.canonicalize().unwrap()
        );
    }
    let git = run(Command::new("git").arg("--exec-path"));
    assert!(
        git.status.success(),
        "Git for Windows --exec-path failed: {}",
        String::from_utf8_lossy(&git.stderr)
    );
    let git = String::from_utf8(git.stdout).unwrap();
    let bash = Path::new(git.trim())
        .ancestors()
        .map(|p| p.join("bin/bash.exe"))
        .find(|p| p.is_file())
        .expect("Install Git for Windows with bin/bash.exe for bootstrap tests");
    // Start with Bash source text, as typed in Git Bash. MSYS does not use the
    // CRT argv parser that Rust's Windows Command quotes for native executables.
    let bash_invocation = format!(
        "exec bash '{}' {}\n",
        root.join("scripts/rsiversi.sh")
            .to_string_lossy()
            .replace('\\', "/")
            .replace('\'', "'\\''"),
        arguments
            .iter()
            .map(|arg| format!("'{}'", arg.replace('\'', "'\\''")))
            .collect::<Vec<_>>()
            .join(" ")
    );
    fs::write(root.join("invoke.sh"), bash_invocation).unwrap();
    let output = run(Command::new(&bash)
        .arg(root.join("invoke.sh"))
        .env_remove("BASH_ENV")
        .current_dir(&caller)
        .env("RSI_SOURCE_PROBE_CWD", &cwd_report));
    assert_eq!(
        output.status.code(),
        Some(37),
        "Git Bash: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        fs::read_to_string(&cwd_report).unwrap(),
        caller.to_str().unwrap()
    );
    assert_eq!(
        String::from_utf8(output.stdout)
            .unwrap()
            .lines()
            .collect::<Vec<_>>(),
        expected
    );
    for operation in ["web", "tui", "desktop"] {
        for flag in ["--help", "-h"] {
            let expected = if operation == "desktop" {
                format!("{}\n", hex(flag))
            } else {
                format!("{}\n{}\n", hex(operation), hex(flag))
            };
            for shell in [powershell(), PathBuf::from("pwsh.exe")] {
                let output = run(Command::new(shell)
                    .args(["-NoProfile", "-ExecutionPolicy", "Bypass", "-File"])
                    .arg(root.join("scripts/rsiversi.ps1"))
                    .args([operation, flag])
                    .env("WT_SESSION", "source-entry-fixture")
                    .current_dir(&caller)
                    .env("RSI_SOURCE_PROBE_CWD", &cwd_report));
                assert_eq!(output.status.code(), Some(37));
                assert_eq!(
                    String::from_utf8(output.stdout)
                        .unwrap()
                        .replace("\r\n", "\n"),
                    expected
                );
            }
            let output = run(Command::new(&bash)
                .arg(root.join("scripts/rsiversi.sh"))
                .args([operation, flag])
                .env_remove("BASH_ENV")
                .env("WT_SESSION", "source-entry-fixture")
                .current_dir(&caller)
                .env("RSI_SOURCE_PROBE_CWD", &cwd_report));
            assert_eq!(output.status.code(), Some(37));
            assert_eq!(
                String::from_utf8(output.stdout)
                    .unwrap()
                    .replace("\r\n", "\n"),
                expected
            );
        }
    }
    for (operation, target) in [("tui", "web"), ("tui", "desktop"), ("web", "desktop")] {
        for (shell, flags, script) in [
            (
                powershell(),
                vec!["-NoProfile", "-ExecutionPolicy", "Bypass", "-File"],
                "rsiversi.ps1",
            ),
            (
                PathBuf::from("pwsh.exe"),
                vec!["-NoProfile", "-ExecutionPolicy", "Bypass", "-File"],
                "rsiversi.ps1",
            ),
            (bash.clone(), vec![], "rsiversi.sh"),
        ] {
            let selected = root.join("selected-executable");
            let output = run(Command::new(shell)
                .args(flags)
                .arg(root.join("scripts").join(script))
                .args([operation, "--from", target, "--", "--help"])
                .env("WT_SESSION", "source-entry-fixture")
                .env("RSI_SOURCE_PROBE_PATH", &selected)
                .env_remove("BASH_ENV")
                .current_dir(&caller)
                .env("RSI_SOURCE_PROBE_CWD", &cwd_report));
            assert_eq!(
                output.status.code(),
                Some(37),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            assert_eq!(
                String::from_utf8(output.stdout)
                    .unwrap()
                    .replace("\r\n", "\n"),
                format!("{}\n{}\n", hex(operation), hex("--help"))
            );
            assert_eq!(
                PathBuf::from(fs::read_to_string(selected).unwrap())
                    .canonicalize()
                    .unwrap(),
                root.join("target/rsi-app")
                    .join(target)
                    .join("generations/00000000000000000001/bundle/rsi.exe")
                    .canonicalize()
                    .unwrap()
            );
        }
    }
    // A stale global publication must never rescue a missing target publication.
    fs::write(
        root.join("target/rsi-app/current"),
        b"00000000000000000001\n",
    )
    .unwrap();
    fs::remove_file(root.join("target/rsi-app/web/current")).unwrap();
    for (shell, flags, script) in [
        (
            powershell(),
            vec!["-NoProfile", "-ExecutionPolicy", "Bypass", "-File"],
            "rsiversi.ps1",
        ),
        (
            PathBuf::from("pwsh.exe"),
            vec!["-NoProfile", "-ExecutionPolicy", "Bypass", "-File"],
            "rsiversi.ps1",
        ),
        (bash.clone(), vec![], "rsiversi.sh"),
    ] {
        let output = run(Command::new(shell)
            .args(flags)
            .arg(root.join("scripts").join(script))
            .args(["path", "web"])
            .env_remove("BASH_ENV"));
        assert!(!output.status.success());
        assert!(String::from_utf8_lossy(&output.stderr).contains("build web"));
    }
    for record in [
        "../outside\n",
        "5\n",
        "999999999999999999999999\n",
        "00000000000000000001",
        "00000000000000000001\r\n",
        "00000000000000000001\nextra",
    ] {
        fs::write(root.join("target/rsi-app/web/current"), record).unwrap();
        for shell in [powershell(), PathBuf::from("pwsh.exe")] {
            let output = run(Command::new(shell)
                .args(["-NoProfile", "-ExecutionPolicy", "Bypass", "-File"])
                .arg(root.join("scripts/rsiversi.ps1"))
                .args(["path", "web"]));
            assert!(!output.status.success(), "accepted {record:?}");
            assert!(output.stdout.is_empty());
        }
        let output = run(Command::new(&bash)
            .args(["./scripts/rsiversi.sh", "path", "web"])
            .env_remove("BASH_ENV")
            .current_dir(&root));
        assert!(!output.status.success(), "Git Bash accepted {record:?}");
        assert!(output.stdout.is_empty());
    }
}
