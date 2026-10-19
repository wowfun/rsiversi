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
    let capture = Capture::open(cache)?;
    write_report(cache, report, |report, stage| {
        let binary =
            capture.build_example(cache, report, "workbench-addon", "paired-workbench-addon")?;
        *stage = "artifact";
        let receipt = json!({"family":capture.family,"binary_sha256":hash_file(&binary)?,"bundle":capture.bundle});
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
            .env("RSI_WEB_ASSETS", capture.bundle.join("assets"))
            .env("RSI_WORKBENCH_REPORT", report.join("browser"))
            .status()?;
        drop(capture);
        if !status.success() {
            return Err("paired addon browser verification failed".into());
        }
        Ok(receipt)
    })
    .map(|_| ())
}

struct Capture {
    bundle: super::PathBuf,
    source: super::PathBuf,
    manifest: Value,
    family: String,
    _pin: fs::File,
}
impl Capture {
    fn open(cache: &Path) -> Result<Self> {
        let published = cache.join("web");
        let id = publication::current(&published)?
            .ok_or("build a successful Web publication first: scripts/rsiversi.sh build web")?;
        let bundle = published.join("generations").join(&id).join("bundle");
        let pin = fs::File::open(published.join("leases").join(id))?;
        pin.try_lock_shared()?;
        let manifest_path = bundle.join("build-family.json");
        let bytes = manifest_bytes(&manifest_path)?;
        let manifest: Value = serde_json::from_slice(&bytes)?;
        let family = hash(&bytes);
        let receipt = read_manifest(&bundle.join("receipt.json"))?;
        if receipt["format"] != 2
            || receipt["family_sha256"] != family
            || receipt["kind"] != "web"
            || receipt["mode"] != "published"
            || manifest["worker_target"] != "wasm32-unknown-unknown"
        {
            return Err(
                "current publication must be a published Web bundle matching the cached capture and wasm32 worker; rebuild web first".into(),
            );
        }
        let source = cache.join("build/web/source");
        source::verify(&source, &manifest["files"])?;
        Ok(Self {
            bundle,
            source,
            manifest,
            family,
            _pin: pin,
        })
    }
    fn build_example(
        &self,
        cache: &Path,
        report: &Path,
        example: &str,
        name: &str,
    ) -> Result<super::PathBuf> {
        let manifest = &self.manifest;
        let family = &self.family;
        let manifest_path = self.bundle.join("build-family.json");
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
            cache.join("build/web/target").into_os_string(),
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
        let mut runner = Runner::new(
            self.source.clone(),
            environment,
            report.join("build.log"),
            false,
        )?;
        let mut command = vec![
            "build",
            "--locked",
            "--target",
            target,
            "-p",
            "rsi-cli",
            "--example",
            example,
        ];
        let default_features = manifest["default_features"]
            .as_bool()
            .ok_or("missing captured default features")?;
        let features = manifest["features"]
            .as_array()
            .ok_or("missing captured Cargo features")?
            .iter()
            .map(|v| v.as_str().ok_or("invalid captured Cargo feature"))
            .collect::<std::result::Result<Vec<_>, _>>()?
            .join(",");
        if !default_features {
            command.push("--no-default-features");
        }
        if !features.is_empty() {
            command.extend(["--features", &features]);
        }
        if profile == "release" {
            command.push("--release");
        }
        runner.run("Compile paired fixture", "cargo", &command)?;
        source::verify(&self.source, &manifest["files"])?;
        let binary = report.join(name);
        fs::copy(
            cache
                .join("build/web/target")
                .join(target)
                .join(profile)
                .join("examples")
                .join(example),
            &binary,
        )?;
        Ok(binary)
    }
}

pub(super) fn build_probe(cache: &Path, report: &Path) -> Result<()> {
    if !cfg!(target_os = "linux") {
        return Err("paired probe requires Linux".into());
    }
    require_new_report(report)?;
    super::validate_external_output(cache, report)?;
    let capture = Capture::open(cache)?;
    write_report(cache, report, |report, stage| {
        let binary =
            capture.build_example(cache, report, "profile-leaf-probe", "profile-leaf-probe")?;
        *stage = "artifact";
        Ok(json!({"binary":binary, "binary_sha256":hash_file(&binary)?, "bundle":capture.bundle, "family":capture.family}))
    })
    .map(|receipt| println!("{receipt}"))
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
) -> Result<Value> {
    require_new_report(report)?;
    let report = super::claim_external_output(cache, report, |path| Ok(fs::create_dir(path)?))?;
    let mut stage = "build";
    let result = run(&report, &mut stage).and_then(|receipt| {
        stage = "receipt";
        let bytes = serde_json::to_vec_pretty(&receipt)?;
        let mut temporary = tempfile::NamedTempFile::new_in(&report)?;
        std::io::Write::write_all(&mut temporary, &bytes)?;
        temporary.persist(report.join("fixture-receipt.json"))?;
        Ok(receipt)
    });
    if let Err(error) = &result {
        if let Err(evidence) = fs::write(
            report.join("failure.json"),
            json!({"stage":stage,"error_type":"addon-fixture"}).to_string(),
        ) {
            diagnostic!("failure evidence: {evidence}");
        }
        return Err(format!("{error}; evidence: {}", report.display()).into());
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(target_os = "linux")]
    #[test]
    fn invalid_probe_report_precedes_capture_access() {
        let cache = tempfile::tempdir().unwrap();
        let report = cache.path().join("invalid-report");
        let error = build_probe(cache.path(), &report).unwrap_err().to_string();
        assert!(
            error.contains("outside the managed publication cache"),
            "{error}"
        );
        assert!(!report.exists());
    }
    #[test]
    fn capture_requires_published_web_identity_and_worker_before_report_creation() {
        let root = tempfile::tempdir().unwrap();
        let cache = root.path();
        let bundle = cache.join("web/generations/00000000000000000001/bundle");
        fs::create_dir_all(&bundle).unwrap();
        fs::create_dir_all(cache.join("web/leases")).unwrap();
        fs::create_dir_all(cache.join("build/web/source")).unwrap();
        fs::write(cache.join("web/current"), "00000000000000000001\n").unwrap();
        fs::write(cache.join("web/leases/00000000000000000001"), "").unwrap();
        for (kind, mode, worker, valid) in [
            ("web", "published", Some("wasm32-unknown-unknown"), true),
            ("tui", "published", Some("wasm32-unknown-unknown"), false),
            (
                "desktop",
                "published",
                Some("wasm32-unknown-unknown"),
                false,
            ),
            ("web", "dev", Some("wasm32-unknown-unknown"), false),
            ("web", "published", None, false),
            ("web", "published", Some("x86_64-unknown-linux-gnu"), false),
        ] {
            let manifest =
                serde_json::to_vec(&json!({"files":{}, "worker_target":worker})).unwrap();
            fs::write(bundle.join("build-family.json"), &manifest).unwrap();
            fs::write(
                bundle.join("receipt.json"),
                json!({"format":2,"family_sha256":hash(&manifest),"kind":kind,"mode":mode})
                    .to_string(),
            )
            .unwrap();
            assert_eq!(
                Capture::open(cache).is_ok(),
                valid,
                "{kind} {mode} {worker:?}"
            );
            #[cfg(target_os = "linux")]
            if !valid {
                let report = cache.join("unused-report");
                assert!(build_probe(cache, &report).is_err());
                assert!(!report.exists());
            }
        }
    }

    #[test]
    fn report_only_commits_success_after_verification_and_preserves_failures() {
        let root = tempfile::tempdir().unwrap();
        let cache = root.path().join("cache");
        fs::create_dir(&cache).unwrap();
        for phase in ["build", "receipt", "artifact", "browser-verification"] {
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
            kind: super::super::Kind::AddonWorkbench,
            with_ssh_helper: false,
            verbose: false,
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
            error.to_string().contains(
                "build a successful Web publication first: scripts/rsiversi.sh build web"
            )
        );
        assert!(!report.exists());
    }
}
