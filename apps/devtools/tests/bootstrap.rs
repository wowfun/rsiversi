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
fn doctor_reports_missing_tools_before_rust_or_node_can_run() {
    let root = tempfile::tempdir().unwrap();
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    for name in ["rsi.ps1", "Cargo.lock", "rust-toolchain.toml"] {
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
        .arg(root.path().join("rsi.ps1"))
        .arg("doctor")
        .env("PATH", root.path())
        .env_remove("ProgramFiles(x86)"));
    assert_eq!(output.status.code(), Some(1));
    let diagnostic = String::from_utf8_lossy(&output.stderr);
    for expected in [
        "Missing git.exe",
        "Missing rustup.exe",
        "Missing node.exe",
        "MSVC x64",
    ] {
        assert!(diagnostic.contains(expected), "{diagnostic}");
    }
    assert!(!root.path().join("target").exists());
}

#[test]
fn doctor_accumulates_non_checkout_and_missing_tools_in_both_powershells() {
    let root = tempfile::tempdir().unwrap();
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    fs::copy(source.join("rsi.ps1"), root.path().join("rsi.ps1")).unwrap();
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
            .arg(root.path().join("rsi.ps1"))
            .arg("doctor")
            .env("PATH", root.path())
            .env_remove("ProgramFiles(x86)"));
        assert_eq!(output.status.code(), Some(1));
        let diagnostic = String::from_utf8_lossy(&output.stderr);
        for expected in [
            "source ZIP archives are unsupported",
            "Missing rustup.exe",
            "Missing node.exe",
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
    fs::create_dir_all(&root).unwrap();
    assert!(root.as_os_str().len() > 100);
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    for name in ["rsi.ps1", "rsi.sh"] {
        fs::copy(source.join(name), root.join(name)).unwrap();
    }
    let bundle = root.join("target/rsi-app/generations/00000000000000000001/bundle");
    fs::create_dir_all(&bundle).unwrap();
    fs::write(
        root.join("target/rsi-app/current"),
        b"00000000000000000001\n",
    )
    .unwrap();
    fs::write(bundle.join("receipt.json"), b"{}").unwrap();
    fs::write(bundle.join(".rsi-generation.lock"), b"").unwrap();
    let fixture = root.join("probe.rs");
    fs::write(&fixture, "fn main(){for arg in std::env::args().skip(1){println!(\"{}\",arg.as_bytes().iter().map(|b|format!(\"{b:02x}\")).collect::<String>());}std::process::exit(37)}").unwrap();
    assert!(
        run(Command::new("rustc")
            .args(["--crate-name", "source_entry_probe"])
            .arg(&fixture)
            .arg("-o")
            .arg(bundle.join("rsi.exe")))
        .status
        .success()
    );
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
        root.join("rsi.ps1").display(),
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
            .current_dir(&root));
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
        let path_script = root.join("path.ps1");
        let mut bytes = vec![0xef, 0xbb, 0xbf];
        bytes.extend_from_slice(format!(
            "$p = & '{}' path\n[Console]::WriteLine([BitConverter]::ToString([Text.Encoding]::UTF8.GetBytes($p)).Replace('-', '').ToLowerInvariant())\n",
            root.join("rsi.ps1").display(),
        ).as_bytes());
        fs::write(&path_script, bytes).unwrap();
        let output = run(Command::new(&shell)
            .args(["-NoProfile", "-ExecutionPolicy", "Bypass", "-File"])
            .arg(path_script));
        assert!(output.status.success(), "{shell:?}");
        let expected_path = hex(&bundle.to_str().unwrap().replace('/', "\\"));
        assert_eq!(
            String::from_utf8(output.stdout).unwrap().trim(),
            expected_path
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
        "exec bash ./rsi.sh {}\n",
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
        .current_dir(&root));
    assert_eq!(
        output.status.code(),
        Some(37),
        "Git Bash: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8(output.stdout)
            .unwrap()
            .lines()
            .collect::<Vec<_>>(),
        expected
    );
    for record in [
        "../outside\n",
        "5\n",
        "999999999999999999999999\n",
        "00000000000000000001",
        "00000000000000000001\r\n",
        "00000000000000000001\nextra",
    ] {
        fs::write(root.join("target/rsi-app/current"), record).unwrap();
        for shell in [powershell(), PathBuf::from("pwsh.exe")] {
            let output = run(Command::new(shell)
                .args(["-NoProfile", "-ExecutionPolicy", "Bypass", "-File"])
                .arg(root.join("rsi.ps1"))
                .arg("path"));
            assert!(!output.status.success(), "accepted {record:?}");
            assert!(output.stdout.is_empty());
        }
        let output = run(Command::new(&bash)
            .args(["./rsi.sh", "path"])
            .env_remove("BASH_ENV")
            .current_dir(&root));
        assert!(!output.status.success(), "Git Bash accepted {record:?}");
        assert!(output.stdout.is_empty());
    }
}
