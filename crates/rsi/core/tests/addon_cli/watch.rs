use super::*;
use std::{
    io::{BufRead as _, Read as _},
    process::{Child, Stdio},
    sync::mpsc,
    time::{Duration, Instant},
};

const GOOD: &str = "printf x >> runs\ncp input artifact.bin\n";
fn setup(root: &Path) -> String {
    fs::write(root.join("input"), b"A").unwrap();
    fs::write(root.join("build.sh"), GOOD).unwrap();
    let text = format!(
        "format = 1\nid = 'fixture.watch'\nplugin = 'fixture.watch'\ntarget = '{}'\nartifact = 'artifact.bin'\n[build]\ncommand = ['/bin/sh', 'build.sh']\nwatch = ['input', 'build.sh']\n",
        rsi::native_addon_target()
    );
    fs::write(root.join("addon.toml"), &text).unwrap();
    text
}
struct Watching {
    child: Child,
    events: Option<mpsc::Receiver<Value>>,
    reader: Option<std::thread::JoinHandle<()>>,
}
impl Watching {
    fn start(root: &Path, read_output: bool, extra: &[&str]) -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_rsi"))
            .env_clear()
            .env("HOME", root.join("home"))
            .current_dir(root)
            .args(["addon", "watch", "addon.toml", "--output", "json", "--root"])
            .arg(root.join("store"))
            .args(extra)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let (events, reader) = if read_output {
            let stdout = child.stdout.take().unwrap();
            let (send, receive) = mpsc::channel();
            let reader = std::thread::spawn(move || {
                for line in std::io::BufReader::new(stdout).lines() {
                    let Ok(line) = line else {
                        break;
                    };
                    let Ok(value) = serde_json::from_str(&line) else {
                        break;
                    };
                    if send.send(value).is_err() {
                        break;
                    }
                }
            });
            (Some(receive), Some(reader))
        } else {
            (None, None)
        };
        Self {
            child,
            events,
            reader,
        }
    }
    fn event(&self) -> Value {
        self.events
            .as_ref()
            .unwrap()
            .recv_timeout(Duration::from_secs(8))
            .expect("watch event")
    }
    fn silent(&self) {
        assert!(matches!(
            self.events
                .as_ref()
                .unwrap()
                .recv_timeout(Duration::from_millis(1200)),
            Err(mpsc::RecvTimeoutError::Timeout)
        ));
    }
    fn exit(&mut self) -> std::process::ExitStatus {
        let until = Instant::now() + Duration::from_secs(8);
        loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                return status;
            }
            assert!(Instant::now() < until, "watch did not settle");
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    fn signal(&self, signal: i32) {
        // SAFETY: this is the still-owned test child PID, with a constant Unix signal.
        #[expect(
            unsafe_code,
            reason = "send an actual signal to the isolated CLI child"
        )]
        let result = unsafe { libc::kill(self.child.id().try_into().unwrap(), signal) };
        assert_eq!(result, 0);
    }
}
impl Drop for Watching {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        self.events.take();
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
    }
}
#[test]
fn watch_builds_changed_inputs_suspends_invalid_edits_and_never_undoes_external_disable() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let manifest = setup(&root);
    let mut watch = Watching::start(&root, true, &["--enable"]);
    let first = watch.event();
    assert_eq!(first["status"], "succeeded");
    assert_eq!(first["enabled"]["revision"], "2");
    watch.silent();
    assert_eq!(fs::read(root.join("runs")).unwrap(), b"x");
    fs::write(root.join("input"), b"B").unwrap();
    let next = watch.event();
    assert_eq!(next["enabled"]["revision"], "4");
    assert_ne!(
        first["installed"]["record"]["artifact_sha256"],
        next["installed"]["record"]["artifact_sha256"]
    );
    fs::write(root.join("build.sh"), "printf f >> runs\nexit 9\n").unwrap();
    let failure = watch.event();
    assert_eq!(failure["status"], "failed");
    assert!(failure["installed"].is_null());
    watch.silent();
    assert_eq!(fs::read(root.join("runs")).unwrap(), b"xxf");
    fs::write(root.join("addon.toml"), b"not valid TOML").unwrap();
    assert_eq!(watch.event()["kind"], "native_addon_watch_error");
    watch.silent();
    fs::write(root.join("build.sh"), GOOD).unwrap();
    fs::write(root.join("addon.toml"), manifest).unwrap();
    assert_eq!(watch.event()["status"], "succeeded");
    let store = rsi::NativeAddonStore::open(root.join("store")).unwrap();
    store.disable("fixture.watch").unwrap();
    fs::write(root.join("input"), b"C").unwrap();
    assert!(!watch.exit().success());
    assert!(store.snapshot().unwrap().enabled.is_empty());
    let mut diagnostic = String::new();
    watch
        .child
        .stderr
        .take()
        .unwrap()
        .read_to_string(&mut diagnostic)
        .unwrap();
    assert!(
        diagnostic.contains("enabled selection changed")
            || diagnostic.contains("native addon source changed"),
        "{diagnostic}"
    );
}
#[test]
fn signal_interrupts_unread_watch_output_and_preserves_published_receipt() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    setup(&root);
    fs::write(
        root.join("build.sh"),
        "cp input artifact.bin\nhead -c 70000 /dev/zero\nhead -c 70000 /dev/zero >&2\n",
    )
    .unwrap();
    let mut watch = Watching::start(&root, false, &[]);
    let until = Instant::now() + Duration::from_secs(8);
    loop {
        if let Ok(Some(snapshot)) = rsi::NativeAddonStore::read_snapshot(&root.join("store"))
            && snapshot.revision == 1
        {
            break;
        }
        assert!(Instant::now() < until, "build did not publish");
        std::thread::sleep(Duration::from_millis(10));
    }
    // A hex report containing two full 64 KiB tails exceeds the unread pipe.
    std::thread::sleep(Duration::from_millis(100));
    assert!(watch.child.try_wait().unwrap().is_none());
    watch.signal(libc::SIGINT);
    assert_eq!(watch.exit().code(), Some(130));
    let store = rsi::NativeAddonStore::open(root.join("store")).unwrap();
    assert_eq!(store.snapshot().unwrap().revision, 1);
    assert!(store.snapshot().unwrap().enabled.is_empty());
    let lock = fs::File::open(&root).unwrap();
    lock.try_lock().unwrap();
    lock.unlock().unwrap();
}

#[test]
fn disable_during_build_keeps_install_receipt_without_reenabling() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    setup(&root);
    let mut watch = Watching::start(&root, true, &["--enable"]);
    assert_eq!(watch.event()["enabled"]["revision"], "2");
    fs::write(root.join("input"), b"B").unwrap();
    fs::write(
        root.join("build.sh"),
        "touch building\nwhile [ ! -f release ]; do sleep 0.02; done\ncp input artifact.bin\n",
    )
    .unwrap();
    let until = Instant::now() + Duration::from_secs(8);
    while !root.join("building").exists() {
        assert!(Instant::now() < until, "second build did not start");
        std::thread::sleep(Duration::from_millis(10));
    }
    let store = rsi::NativeAddonStore::open(root.join("store")).unwrap();
    assert_eq!(store.disable("fixture.watch").unwrap().revision, 3);
    fs::write(root.join("release"), []).unwrap();
    let report = watch.event();
    assert_eq!(report["status"], "succeeded");
    assert_eq!(report["installed"]["revision"], "4");
    assert!(report["enabled"].is_null());
    assert_eq!(report["enable_error"], "native addon source changed");
    assert!(!watch.exit().success());
    let source = store.snapshot().unwrap();
    assert_eq!(source.revision, 4);
    assert!(source.enabled.is_empty());
}

#[test]
fn watch_exits_on_source_directory_loss_and_rejects_missing_watch_declaration() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let source = root.join("source");
    fs::create_dir(&source).unwrap();
    let manifest = setup(&source);
    let mut watch = Watching::start(&source, true, &[]);
    assert_eq!(watch.event()["status"], "succeeded");
    fs::rename(&source, root.join("moved")).unwrap();
    assert!(!watch.exit().success());
    fs::create_dir(&source).unwrap();
    fs::write(
        source.join("addon.toml"),
        manifest.replace("watch = ['input', 'build.sh']", "watch = []"),
    )
    .unwrap();
    let mut watch = Watching::start(&source, true, &[]);
    assert!(!watch.exit().success());
    assert!(!source.join("store").exists());
}
