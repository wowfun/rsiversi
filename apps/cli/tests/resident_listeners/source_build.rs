use super::*;
use sha2::{Digest as _, Sha256};
use std::{path::Path, time::Duration};

fn package(source: &Path, bytes: &[u8]) {
    std::fs::write(source.join("input"), bytes).unwrap();
    std::fs::write(source.join("manifest"),serde_json::to_vec(&serde_json::json!({
        "format":1,"name":"fixture-build","version":"fixture","target":"portable","dependencies":[],"requires":[],
        "files":[{"path":"payload","bytes":bytes.len(),"digest":hex::encode(Sha256::digest(bytes)),"executable":false}],
        "exports":{"main":{"kind":"application_profile","path":"payload"}}
    })).unwrap()).unwrap();
}
async fn until(mut condition: impl FnMut() -> bool) {
    tokio::time::timeout(Duration::from_secs(15), async {
        while !condition() {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("source observation deadline");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[allow(clippy::too_many_lines)] // Observe one build owner through deduplication, failed replacement, publication and final lease release.
async fn ordinary_watch_deduplicates_sources_preserves_good_package_and_releases_last_lease() {
    let fixture = CliFixture::new("http://127.0.0.1:9");
    install_web(&fixture);
    let source = fixture.workspace.join("source");
    std::fs::create_dir_all(source.join("out")).unwrap();
    package(&source, b"first");
    #[cfg(windows)]
    let (filename, script) = (
        "fixture-build.cmd",
        concat!(
            "@echo off\r\nif defined DEEPSEEK_API_KEY exit /b 8\r\n",
            "if not \"%~1\"==\"literal space & marker\" exit /b 9\r\n",
            "echo build>>count\r\nif exist fail exit /b 7\r\n",
            "if exist slow powershell.exe -NoProfile -NonInteractive -Command \"$PID | Set-Content build-pid; Start-Sleep -Seconds 30\"\r\n",
            "copy /y input out\\payload >nul\r\ncopy /y manifest out\\package.json >nul\r\n"
        ),
    );
    #[cfg(unix)]
    let (filename, script) = (
        "fixture-build",
        concat!(
            "#!/bin/sh\nset -e\n[ -z \"${DEEPSEEK_API_KEY+x}\" ]\n",
            "[ \"$(basename \"$0\")\" = fixture-build ]\n",
            "[ \"$1\" = 'literal space & marker' ]\nprintf 'build\\n' >>count\n[ ! -e fail ]\n",
            "if [ -e slow ]; then echo $$ > build-pid; sleep 30; fi\n",
            "cp input out/payload\ncp manifest out/package.json\n"
        ),
    );
    std::fs::write(source.join(filename), script).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(
            source.join(filename),
            std::fs::Permissions::from_mode(0o700),
        )
        .unwrap();
        std::fs::rename(source.join(filename), source.join("build-implementation")).unwrap();
        std::os::unix::fs::symlink("build-implementation", source.join(filename)).unwrap();
    }
    let config = fixture.temporary.path().join("config/rsi/rsi.toml");
    let mut value = rsi::LaunchConfiguration::parse(&std::fs::read(&config).unwrap()).unwrap();
    value.sources.insert(
        "fixture-build".into(),
        rsi::LaunchSource {
            frontend: None,
            directory: source.clone(),
            command: vec!["fixture-build".into(), "literal space & marker".into()],
            output: "out/package.json".into(),
            inputs: [
                "input",
                "manifest",
                "fail",
                "slow",
                if cfg!(unix) {
                    "build-implementation"
                } else {
                    filename
                },
            ]
            .map(Into::into)
            .into(),
        },
    );
    std::fs::write(&config, toml::to_string(&value).unwrap()).unwrap();
    // Runtime publication can copy a full default-debug ELF before the Host is
    // ready. Keep that separate from the source-observation deadline below.
    fixture.assert_success(&["host", "start"]);
    assert!(
        !source.join("count").exists(),
        "ordinary Host startup must not run source recipes"
    );
    let path = std::env::join_paths(
        std::iter::once(source.clone())
            .chain(std::env::split_paths(&std::env::var_os("PATH").unwrap())),
    )
    .unwrap();
    let start = || {
        fixture
            .tokio_command()
            .args(["--watch", "web", "--no-open", "--port", "0"])
            .env("PATH", &path)
            .env("DEEPSEEK_API_KEY", "fixture-not-forwarded")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::inherit())
            .kill_on_drop(true)
            .spawn()
            .unwrap()
    };
    let mut first = start();
    let store =
        rsi_packages::PackageStore::open(&fixture.temporary.path().join("state/rsi/packages"))
            .unwrap();
    until(|| store.acquire("fixture-build").is_ok()).await;
    assert!(first.try_wait().unwrap().is_none());
    let initial = store.acquire("fixture-build").unwrap().digest().to_owned();
    let paths = rsi_host::HostPaths::new(
        fixture.temporary.path().join("config/rsi"),
        fixture.temporary.path().join("state/rsi"),
        fixture.temporary.path().join("cache/rsi"),
    )
    .unwrap();
    let conflicting = rsi::BuildRecipe {
        package: "fixture-build".into(),
        directory: source.clone(),
        command: vec![fixture.binary.to_str().unwrap().into(), "--version".into()],
        output: "out/package.json".into(),
        inputs: vec!["input".into()],
        environment: std::collections::BTreeMap::default(),
    };
    assert!(
        matches!(
            rsi::acquire_source_build(&paths, conflicting).await,
            Err(rsi::SourceBuildError::Busy)
        ),
        "conflicting command cannot acquire a shared output"
    );
    let mut second = start();
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert!(second.try_wait().unwrap().is_none());
    assert_eq!(
        std::fs::read_to_string(source.join("count"))
            .unwrap()
            .lines()
            .count(),
        1,
        "identical callers share a command"
    );
    std::fs::write(source.join("fail"), b"fail").unwrap();
    until(|| {
        std::fs::read_to_string(source.join("count"))
            .unwrap()
            .lines()
            .count()
            >= 2
    })
    .await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(store.acquire("fixture-build").unwrap().digest(), initial);
    first.kill().await.unwrap();
    // A legitimate reader can briefly exclude the package writer. Successful
    // commands must retry that transient refusal without another source edit.
    let reader = std::fs::File::open(
        fixture
            .temporary
            .path()
            .join("state/rsi/packages/writer.lock"),
    )
    .unwrap();
    reader.lock_shared().unwrap();
    package(&source, b"second generation");
    std::fs::remove_file(source.join("fail")).unwrap();
    tokio::time::sleep(Duration::from_millis(900)).await;
    reader.unlock().unwrap();
    until(|| {
        store
            .acquire("fixture-build")
            .is_ok_and(|lease| lease.digest() != initial)
    })
    .await;
    assert!(second.try_wait().unwrap().is_none());
    // A malformed launch edit keeps the attachment and its source recipe alive.
    std::fs::write(&config, "not valid = [").unwrap();
    tokio::time::sleep(Duration::from_millis(700)).await;
    assert!(second.try_wait().unwrap().is_none());
    let before_recipe = std::fs::read(source.join("count")).unwrap();
    value
        .sources
        .get_mut("fixture-build")
        .unwrap()
        .inputs
        .push("added-input".into());
    std::fs::write(&config, toml::to_string(&value).unwrap()).unwrap();
    until(|| std::fs::read(source.join("count")).unwrap() != before_recipe).await;
    let after_recipe = std::fs::read(source.join("count")).unwrap();
    std::fs::write(source.join("added-input"), b"newly watched").unwrap();
    until(|| std::fs::read(source.join("count")).unwrap() != after_recipe).await;
    assert!(second.try_wait().unwrap().is_none());
    second.kill().await.unwrap();
    tokio::time::sleep(Duration::from_millis(500)).await;
    let count = std::fs::read(source.join("count")).unwrap();
    package(&source, b"after all clients exited");
    tokio::time::sleep(Duration::from_millis(800)).await;
    assert_eq!(std::fs::read(source.join("count")).unwrap(), count);
    std::fs::write(source.join("slow"), b"slow").unwrap();
    let mut third = start();
    until(|| {
        std::fs::read_to_string(source.join("build-pid"))
            .ok()
            .is_some_and(|s| s.trim().parse::<u32>().is_ok())
    })
    .await;
    let pid: u32 = std::fs::read_to_string(source.join("build-pid"))
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    #[cfg(windows)]
    let handle = rsi_process_native::ProcessHandle::open(pid).unwrap();
    third.kill().await.unwrap();
    #[cfg(windows)]
    until(|| !handle.is_running().unwrap()).await;
    #[cfg(unix)]
    until(|| !Path::new(&format!("/proc/{pid}")).exists()).await;
    fixture.assert_success(&["host", "stop"]);
}
