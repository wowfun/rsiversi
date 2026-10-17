use super::{
    Command, Options, Path, Result, Runner, Value, environment, fs, hash, hash_file, json,
    manifest_bytes, publication, read_manifest, source,
};

#[allow(clippy::too_many_lines)] // Keep the verified capture, independent build and retained browser lease in order.
pub(super) fn run(
    root: &Path,
    cache: &Path,
    options: &Options,
    build_lock: fs::File,
) -> Result<()> {
    if !cfg!(target_os = "linux") {
        return Err("paired addon browser fixture requires Linux".into());
    }
    let report = options.output.as_ref().ok_or("missing report directory")?;
    require_new_report(report)?;
    let id = publication::current(cache)?.ok_or("build a successful paired publication first")?;
    let bundle = cache.join("generations").join(&id).join("bundle");
    let pin = fs::File::open(cache.join("leases").join(id))?;
    pin.try_lock_shared()?;
    let manifest_path = cache.join("build/build-family.json");
    let bytes = manifest_bytes(&manifest_path)?;
    let manifest: Value = serde_json::from_slice(&bytes)?;
    let family = hash(&bytes);
    let receipt = read_manifest(&bundle.join("receipt.json"))?;
    if receipt["format"] != 2 || receipt["family_sha256"] != family {
        return Err("current publication does not match the cached capture; rebuild first".into());
    }
    let source = cache.join("build/source");
    source::verify(&source, &manifest["files"])?;
    let mut environment = environment(cache)?;
    for name in ["RUSTFLAGS", "CARGO_ENCODED_RUSTFLAGS"] {
        environment.insert(
            name.into(),
            manifest["flags"][name]
                .as_str()
                .ok_or("invalid captured flags")?
                .into(),
        );
    }
    environment.insert(
        "RSI_BUILD_FAMILY_MANIFEST".into(),
        manifest_path.into_os_string(),
    );
    environment.insert("RSI_BUILD_FAMILY_SHA256".into(), family.clone().into());
    environment.insert(
        "CARGO_TARGET_DIR".into(),
        cache.join("build/target").into_os_string(),
    );
    let target = manifest["target"]
        .as_str()
        .ok_or("missing captured target")?;
    let profile = manifest["profile"]
        .as_str()
        .ok_or("missing captured profile")?;
    if !matches!(
        target,
        "x86_64-unknown-linux-gnu" | "aarch64-unknown-linux-gnu"
    ) || !matches!(profile, "release" | "debug")
    {
        return Err("invalid fixture target/profile".into());
    }
    write_report(cache, report, |report, stage| {
        let mut runner = Runner {
            cwd: source.clone(),
            environment,
            log: fs::File::create(report.join("build.log"))?,
            log_path: report.join("build.log"),
        };
        let mut command = vec![
            "build",
            "--locked",
            "--target",
            target,
            "-p",
            "rsi-cli",
            "--example",
            "workbench-addon",
        ];
        if profile == "release" {
            command.push("--release");
        }
        runner.run("cargo", &command)?;
        *stage = "capture-verification";
        source::verify(&source, &manifest["files"])?;
        *stage = "artifact";
        let binary = report.join("paired-workbench-addon");
        fs::copy(
            cache
                .join("build/target")
                .join(target)
                .join(profile)
                .join("examples/workbench-addon"),
            &binary,
        )?;
        let receipt = json!({"family":family,"binary_sha256":hash_file(&binary)?,"bundle":bundle,"command":command});
        drop(build_lock);
        *stage = "browser-verification";
        let mut child = Command::new("node");
        child
            .arg(root.join("fixtures/rsi/addon-workbench/verify.mjs"))
            .current_dir(root)
            .env_clear();
        for key in [
            "PATH",
            "LD_LIBRARY_PATH",
            "PLAYWRIGHT_BROWSERS_PATH",
            "HOME",
        ] {
            if let Some(value) = std::env::var_os(key) {
                child.env(key, value);
            }
        }
        let status = child
            .env("RSI_WORKBENCH_BINARY", binary)
            .env("RSI_WEB_ASSETS", bundle.join("assets"))
            .env("RSI_WORKBENCH_REPORT", report.join("browser"))
            .status()?;
        drop(pin);
        if !status.success() {
            return Err("paired addon browser verification failed".into());
        }
        Ok(receipt)
    })
}

fn require_new_report(report: &Path) -> Result<()> {
    if report.try_exists()? {
        return Err(format!(
            "report directory already exists; choose a new absolute path: {}",
            report.display()
        )
        .into());
    }
    Ok(())
}

fn write_report(
    cache: &Path,
    report: &Path,
    run: impl FnOnce(&Path, &mut &'static str) -> Result<Value>,
) -> Result<()> {
    require_new_report(report)?;
    let report = super::claim_external_output(cache, report, |path| Ok(fs::create_dir(path)?))?;
    let mut stage = "build";
    let result = run(&report, &mut stage).and_then(|receipt| {
        stage = "receipt";
        let bytes = serde_json::to_vec_pretty(&receipt)?;
        let mut temporary = tempfile::NamedTempFile::new_in(&report)?;
        std::io::Write::write_all(&mut temporary, &bytes)?;
        temporary.persist(report.join("fixture-receipt.json"))?;
        Ok(())
    });
    if let Err(error) = result {
        if let Err(evidence) = fs::write(
            report.join("failure.json"),
            json!({"stage":stage,"error_type":"addon-fixture"}).to_string(),
        ) {
            eprintln!("failure evidence: {evidence}");
        }
        return Err(format!("{error}; evidence: {}", report.display()).into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn report_only_commits_success_after_verification_and_preserves_failures() {
        let root = tempfile::tempdir().unwrap();
        let cache = root.path().join("cache");
        fs::create_dir(&cache).unwrap();
        for phase in [
            "build",
            "capture-verification",
            "artifact",
            "browser-verification",
        ] {
            let report = root.path().join(phase);
            let error = write_report(&cache, &report, |report, stage| {
                assert!(!report.join("fixture-receipt.json").exists());
                *stage = phase;
                Err("fixture failure".into())
            })
            .unwrap_err();
            assert!(error.to_string().contains("fixture failure"));
            assert!(!report.join("fixture-receipt.json").exists());
            assert_eq!(
                read_manifest(&report.join("failure.json")).unwrap()["stage"],
                phase
            );
            assert!(
                write_report(&cache, &report, |_, _| panic!(
                    "existing report must not run"
                ))
                .unwrap_err()
                .to_string()
                .contains("choose a new absolute path")
            );
        }
        let report = root.path().join("success");
        write_report(&cache, &report, |report, _| {
            assert!(!report.join("fixture-receipt.json").exists());
            Ok(json!({"family":"verified"}))
        })
        .unwrap();
        assert_eq!(
            read_manifest(&report.join("fixture-receipt.json")).unwrap()["family"],
            "verified"
        );
        assert!(!report.join("failure.json").exists());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn missing_publication_does_not_consume_report_path() {
        let root = tempfile::tempdir().unwrap();
        let cache = root.path().join("cache");
        fs::create_dir(&cache).unwrap();
        let report = root.path().join("report");
        let options = Options {
            kind: "addon-workbench".into(),
            output: Some(report.clone()),
            debug: false,
        };
        let error = run(
            root.path(),
            &cache,
            &options,
            fs::File::create(cache.join("build.lock")).unwrap(),
        )
        .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("build a successful paired publication first")
        );
        assert!(!report.exists());
    }
}
