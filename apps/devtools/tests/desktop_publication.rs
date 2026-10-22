#![cfg(windows)]

use sha2::{Digest, Sha256};
use std::{fs, path::Path, process::Command};

// Exercise the real producer with native fixture tools, without compiling WebView2 or a Worker.
#[test]
#[allow(clippy::too_many_lines)] // Keep tool setup and publication assertions in one isolated fixture.
fn windows_desktop_publication_pairs_exe_names_assets_and_receipt() {
    let root = tempfile::tempdir().unwrap();
    let repo = root.path().join("repo space 中文");
    let tools = root.path().join("tools");
    fs::create_dir_all(repo.join("apps/devtools")).unwrap();
    fs::create_dir_all(repo.join("apps/web")).unwrap();
    fs::create_dir_all(repo.join("target/rsi-tools/pnpm/node_modules/pnpm/bin")).unwrap();
    fs::create_dir(&tools).unwrap();
    fs::write(repo.join("Cargo.toml"), "[workspace]\n").unwrap();
    fs::write(
        repo.join("Cargo.lock"),
        "[[package]]\nname = 'wasm-bindgen'\nversion = '0.2.100'\n",
    )
    .unwrap();
    fs::write(repo.join("apps/devtools/Cargo.toml"), "").unwrap();
    fs::write(
        repo.join("apps/web/package.json"),
        r#"{"packageManager":"pnpm@12.6.0"}"#,
    )
    .unwrap();
    fs::write(
        repo.join("target/rsi-tools/pnpm/node_modules/pnpm/bin/pnpm.mjs"),
        "fixture",
    )
    .unwrap();
    fs::write(repo.join(".gitignore"), "/target/\n").unwrap();
    assert!(
        Command::new("git")
            .args(["init", "-q"])
            .current_dir(&repo)
            .status()
            .unwrap()
            .success()
    );
    let source = tools.join("tool.rs");
    fs::write(&source, r#"
use std::{env, fs, path::PathBuf};
fn main() {
    let exe = env::current_exe().unwrap();
    let tool = exe.file_stem().unwrap().to_str().unwrap();
    let args: Vec<_> = env::args().skip(1).collect();
    match tool {
        "rustc" => println!("rustc fixture\nhost: x86_64-pc-windows-msvc"),
        "cargo" if args == ["-V"] => println!("cargo fixture"),
        "cargo" => {
            assert!(args.windows(2).any(|a| a == ["-p", "rsi-cli"]));
            assert!(args.windows(2).any(|a| a == ["-p", "rsi-desktop"]));
            assert!(!args.iter().any(|a| a == "rsi-ssh-helper-app"));
            let profile = if args.iter().any(|a| a == "--release") { "release" } else { "debug" };
            let target = &args[args.iter().position(|a| a == "--target").unwrap() + 1];
            let out = PathBuf::from(env::var_os("CARGO_TARGET_DIR").unwrap()).join(target).join(profile);
            fs::create_dir_all(&out).unwrap();
            for binary in ["rsi.exe", "rsi-desktop.exe"] { fs::write(out.join(binary), binary).unwrap(); }
        },
        "wasm-bindgen" => println!("wasm-bindgen 0.2.100"),
        "node" if args == ["--version"] => println!("v26.5.0"),
        "node" if args.iter().any(|a| a == "--version") => println!("12.6.0"),
        "node" if args.first().map(String::as_str) == Some("apps/web/build.mjs") => {
            let out = PathBuf::from(&args[1]);
            fs::create_dir_all(&out).unwrap();
            fs::write(out.join("index.html"), "fixture document").unwrap();
        },
        "node" => assert!(args.iter().any(|a| a == "install")),
        _ => panic!("unexpected tool: {tool}")
    }
}
"#).unwrap();
    let executable = tools.join("fixture.exe");
    let compiled = Command::new("rustc")
        .arg(&source)
        .args(["--edition=2024", "-o"])
        .arg(&executable)
        .output()
        .unwrap();
    assert!(
        compiled.status.success(),
        "{}",
        String::from_utf8_lossy(&compiled.stderr)
    );
    for tool in ["rustc", "cargo", "node", "wasm-bindgen"] {
        fs::copy(&executable, tools.join(format!("{tool}.exe"))).unwrap();
    }
    let path = std::env::join_paths(
        std::iter::once(tools.clone())
            .chain(std::env::split_paths(&std::env::var_os("PATH").unwrap())),
    )
    .unwrap();
    for debug in [false, true] {
        let mut command = Command::new(env!("CARGO_BIN_EXE_rsi-app-tools"));
        command
            .args(["fixture-dist", "desktop"])
            .current_dir(&repo)
            .env("PATH", &path)
            .env_remove("RSI_WASM_BINDGEN");
        if debug {
            command.arg("--debug");
        }
        let output = command.output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        let bundle = Path::new(result["bundle"].as_str().unwrap());
        let receipt: serde_json::Value =
            serde_json::from_slice(&fs::read(bundle.join("receipt.json")).unwrap()).unwrap();
        let bytes = fs::read(bundle.join("build-family.json")).unwrap();
        let manifest: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(receipt["kind"], "desktop");
        assert_eq!(receipt["mode"], "published");
        assert_eq!(receipt["profile"], if debug { "debug" } else { "release" });
        assert_eq!(receipt["target"], "x86_64-pc-windows-msvc");
        assert!(receipt["helper_target"].is_null());
        assert_eq!(
            receipt["family_sha256"],
            format!("{:x}", Sha256::digest(bytes))
        );
        assert_eq!(manifest["worker_target"], "wasm32-unknown-unknown");
        assert_eq!(
            manifest["packages"],
            serde_json::json!(["rsi-cli", "rsi-web", "rsi-desktop"])
        );
        for name in ["rsi.exe", "rsi-desktop.exe", "assets/index.html"] {
            assert_eq!(
                receipt["artifacts"][name],
                format!("{:x}", Sha256::digest(fs::read(bundle.join(name)).unwrap()))
            );
        }
        assert!(!bundle.join("rsi-ssh-helper").exists());
        assert!(!bundle.join("rsi-desktop").exists());
        assert_eq!(
            fs::read_to_string(repo.join("target/rsi-app/desktop/current"))
                .unwrap()
                .trim(),
            bundle
                .parent()
                .unwrap()
                .file_name()
                .unwrap()
                .to_str()
                .unwrap()
        );
        assert!(!repo.join("target/rsi-app/web/current").exists());
    }
}
