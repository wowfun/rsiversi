//! One leased build loop per frozen recipe, using the Host's ordinary providers.
use rsi_files_native_fs::{
    Directory, open_absolute_directory_no_follow, open_relative_file_no_follow,
};
use rsi_host_control::{BuildRecipe, BuildUpdate};
use rsi_process::{Process, ProcessSpec};
use rsi_sandbox::{Sandbox, SandboxMode};
use std::{
    collections::BTreeMap,
    io::Read as _,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::sync::watch;
use tokio_util::{sync::CancellationToken, task::TaskTracker};

type Result<T> = std::result::Result<T, &'static str>;
pub(super) enum AcquireError {
    Busy,
    Invalid(&'static str),
}
impl From<&'static str> for AcquireError {
    fn from(message: &'static str) -> Self {
        Self::Invalid(message)
    }
}
struct Captured {
    recipe: BuildRecipe,
    directory: Directory,
    output: PathBuf,
    output_key: PathBuf,
}
use crate::source_inputs::path_key;
impl Captured {
    fn open(recipe: BuildRecipe) -> Result<Self> {
        if rsi_packages::validate_identifier(&recipe.package).is_err()
            || !recipe.directory.is_absolute()
            || recipe.command.is_empty()
            || recipe.command.len() > 128
            || !Path::new(&recipe.command[0]).is_absolute()
            || recipe
                .command
                .iter()
                .any(|v| v.len() > 16 * 1024 || v.contains('\0'))
            || recipe.inputs.is_empty()
            || recipe.inputs.len() > 256
            || recipe.environment.len() > 256
            || recipe.environment.iter().any(|(k, v)| {
                k.is_empty()
                    || k.contains(['=', '\0'])
                    || k.len() > 256
                    || v.contains('\0')
                    || v.len() > 16 * 1024
            })
        {
            return Err("invalid source recipe");
        }
        let scrubbed: BTreeMap<String, String> = crate::scrub_child_environment(
            recipe.environment.iter().map(|(k, v)| (k.into(), v.into())),
        )
        .into_iter()
        .map(|(k, v)| {
            (
                k.into_string().expect("UTF-8 input"),
                v.into_string().expect("UTF-8 input"),
            )
        })
        .collect();
        if scrubbed != recipe.environment {
            return Err("source environment contains forbidden inputs");
        }
        crate::source_inputs::layout(&recipe.output, &recipe.inputs)
            .map_err(|_| "invalid source input/output layout")?;
        let output = recipe.output.parent().expect("validated output directory");
        let directory = open_absolute_directory_no_follow(&recipe.directory)
            .map_err(|_| "source directory unavailable")?;
        let output_key = path_key(
            &std::fs::canonicalize(&recipe.directory)
                .map_err(|_| "source directory unavailable")?
                .join(output),
        );
        let output = recipe.directory.join(output);
        Ok(Self {
            recipe,
            directory,
            output,
            output_key,
        })
    }
    fn metadata_fingerprint(&self) -> Result<String> {
        let current = open_absolute_directory_no_follow(&self.recipe.directory)
            .map_err(|_| "source directory unavailable")?;
        if !same_directory(&self.directory, &current).map_err(|_| "source identity unavailable")? {
            return Err("source directory replaced");
        }
        crate::source_inputs::metadata_fingerprint(&self.directory, &self.recipe.inputs)
            .map_err(|_| "source input capture failed")
    }
    fn fingerprint(&self) -> Result<String> {
        let current = open_absolute_directory_no_follow(&self.recipe.directory)
            .map_err(|_| "source directory unavailable")?;
        if !same_directory(&self.directory, &current).map_err(|_| "source identity unavailable")? {
            return Err("source directory replaced");
        }
        crate::source_inputs::fingerprint(&self.directory, &self.recipe.inputs)
            .map_err(|_| "source input capture failed")
    }
}
#[cfg(unix)]
fn same_directory(a: &Directory, b: &Directory) -> std::io::Result<bool> {
    use std::os::unix::fs::MetadataExt as _;
    let (a, b) = (a.dir_metadata()?, b.dir_metadata()?);
    Ok((a.dev(), a.ino()) == (b.dev(), b.ino()))
}
#[cfg(windows)]
fn same_directory(a: &Directory, b: &Directory) -> std::io::Result<bool> {
    Ok(a.version()?.identity == b.version()?.identity)
}
struct Job {
    captured: Arc<Captured>,
    leases: std::sync::atomic::AtomicUsize,
    stop: CancellationToken,
    updates: watch::Sender<BuildUpdate>,
}
pub(super) struct Lease {
    job: Arc<Job>,
    pub updates: watch::Receiver<BuildUpdate>,
}
impl Drop for Lease {
    fn drop(&mut self) {
        if self
            .job
            .leases
            .fetch_sub(1, std::sync::atomic::Ordering::AcqRel)
            == 1
        {
            self.job.stop.cancel();
        }
    }
}
pub(super) struct Manager {
    process: Arc<dyn Process>,
    sandbox: Arc<dyn Sandbox>,
    packages: PathBuf,
    interfaces: Vec<rsi_packages::Interface>,
    jobs: Mutex<BTreeMap<PathBuf, Arc<Job>>>,
    stop: CancellationToken,
    tasks: TaskTracker,
}
impl Manager {
    pub fn new(
        process: Arc<dyn Process>,
        sandbox: Arc<dyn Sandbox>,
        packages: PathBuf,
        interfaces: Vec<rsi_packages::Interface>,
    ) -> Arc<Self> {
        Arc::new(Self {
            process,
            sandbox,
            packages,
            interfaces,
            jobs: Mutex::new(BTreeMap::new()),
            stop: CancellationToken::new(),
            tasks: TaskTracker::new(),
        })
    }
    pub async fn acquire(
        self: &Arc<Self>,
        recipe: BuildRecipe,
    ) -> std::result::Result<Lease, AcquireError> {
        let captured = Arc::new(
            tokio::task::spawn_blocking(|| Captured::open(recipe))
                .await
                .map_err(|_| "source admission worker failed")??,
        );
        let mut jobs = self.jobs.lock().expect("source builds");
        if self.stop.is_cancelled() {
            return Err("source builds are stopping".into());
        }
        for (output, job) in jobs.iter() {
            if captured.output_key.starts_with(output) || output.starts_with(&captured.output_key) {
                if captured.recipe != job.captured.recipe || job.stop.is_cancelled() {
                    return Err(AcquireError::Busy);
                }
                job.leases
                    .fetch_update(
                        std::sync::atomic::Ordering::AcqRel,
                        std::sync::atomic::Ordering::Acquire,
                        |count| (count > 0 && count < 256).then_some(count + 1),
                    )
                    .map_err(|_| "source lease is closing or at capacity")?;
                return Ok(Lease {
                    job: job.clone(),
                    updates: job.updates.subscribe(),
                });
            }
        }
        if jobs.len() >= 8 {
            return Err("source build capacity exhausted".into());
        }
        let (updates, receiver) = watch::channel(BuildUpdate::Building {});
        let job = Arc::new(Job {
            captured,
            leases: std::sync::atomic::AtomicUsize::new(1),
            stop: self.stop.child_token(),
            updates,
        });
        jobs.insert(job.captured.output_key.clone(), job.clone());
        let owner = self.clone();
        let work = job.clone();
        self.tasks.spawn(async move {
            owner.observe(&work).await;
            owner
                .jobs
                .lock()
                .expect("source builds")
                .remove(&work.captured.output_key);
        });
        Ok(Lease {
            job,
            updates: receiver,
        })
    }
    pub async fn shutdown(&self) {
        self.stop.cancel();
        self.tasks.close();
        self.tasks.wait().await;
    }
    async fn observe(&self, job: &Job) {
        let mut previous = None;
        let mut previous_hint = None;
        let mut last_content_scan = tokio::time::Instant::now();
        loop {
            if job.stop.is_cancelled() {
                break;
            }
            let captured = job.captured.clone();
            let hint = tokio::task::spawn_blocking(move || captured.metadata_fingerprint())
                .await
                .unwrap_or(Err("source scanner failed"));
            if previous_hint.as_ref() == Some(&hint)
                && last_content_scan.elapsed() < Duration::from_secs(5)
            {
                tokio::select! { () = job.stop.cancelled() => break, () = tokio::time::sleep(Duration::from_millis(300)) => {} }
                continue;
            }
            previous_hint = Some(hint);
            last_content_scan = tokio::time::Instant::now();
            let captured = job.captured.clone();
            let current = tokio::task::spawn_blocking(move || captured.fingerprint())
                .await
                .unwrap_or(Err("source scanner failed"));
            if previous.as_ref() != Some(&current) {
                previous = Some(current.clone());
                let outcome = match current {
                    Ok(fingerprint) => {
                        job.updates.send_replace(BuildUpdate::Building {});
                        self.build(job, fingerprint).await
                    }
                    Err(reason) => Err(reason),
                };
                if job.stop.is_cancelled() {
                    break;
                }
                job.updates
                    .send_replace(outcome.unwrap_or_else(|reason| BuildUpdate::Failed {
                        reason: reason.into(),
                    }));
            }
            tokio::select! { () = job.stop.cancelled() => break, () = tokio::time::sleep(Duration::from_millis(300)) => {} }
        }
    }
    async fn build(&self, job: &Job, fingerprint: String) -> Result<BuildUpdate> {
        struct Terminate(rsi_process::ManagedProcess);
        impl Drop for Terminate {
            fn drop(&mut self) {
                self.0.terminate();
            }
        }
        let recipe = &job.captured.recipe;
        let confined = self
            .sandbox
            .confine(rsi_sandbox::ProcessRequest {
                stdio: rsi_sandbox::ProcessStdio::Pipes,
                mode: SandboxMode::DangerFullAccess,
                program: PathBuf::from(&recipe.command[0]),
                arguments: recipe.command[1..].to_vec(),
                cwd: recipe.directory.clone(),
                workspace: recipe.directory.clone(),
            })
            .await
            .map_err(|_| "source Sandbox admission failed")?;
        if job.stop.is_cancelled() {
            return Err("source build cancelled");
        }
        let managed = self
            .process
            .spawn(ProcessSpec {
                process: confined,
                stdin: vec![],
                environment: recipe
                    .environment
                    .iter()
                    .map(|(k, v)| (k.into(), v.into()))
                    .collect(),
                stdout_max_bytes: 64 * 1024,
                stderr_max_bytes: 64 * 1024,
                termination_grace_ms: 100,
            })
            .await
            .map_err(|_| "source process spawn failed")?;
        let _terminate = Terminate(managed.clone());
        let outcome = tokio::select! { biased;
            () = job.stop.cancelled() => { managed.terminate(); let _ = managed.wait().await; return Err("source build cancelled"); },
            () = tokio::time::sleep(Duration::from_mins(30)) => { managed.terminate(); let _ = managed.wait().await; return Err("source build deadline elapsed"); },
            result = managed.wait() => result.map_err(|_| "source process settlement failed")?,
        };
        if outcome.exit_code != Some(0) {
            return Err("source command failed; previous package retained");
        }
        let captured = job.captured.clone();
        let stop = job.stop.clone();
        let packages = self.packages.clone();
        let interfaces = self.interfaces.clone();
        tokio::task::spawn_blocking(move || {
            let check = || {
                if stop.is_cancelled() || captured.fingerprint()? != fingerprint { return Err("source inputs changed before publication"); }
                Ok(())
            };
            check()?;
            let mut manifest = Vec::new();
            open_relative_file_no_follow(&captured.directory,&captured.recipe.output).map_err(|_| "source manifest unavailable")?
                .take(rsi_packages::MAX_DOCUMENT as u64+1).read_to_end(&mut manifest).map_err(|_| "source manifest unreadable")?;
            let parsed = rsi_packages::Manifest::parse(&manifest).map_err(|_| "source manifest invalid")?;
            if parsed.name != captured.recipe.package { return Err("source package identity differs"); }
            let digest = hex::encode(sha2::Sha256::digest(&manifest));
            let store = rsi_packages::PackageStore::open(&packages).map_err(|_| "package store unavailable")?;
            let source = [rsi_packages::InstallSource { directory:captured.output.clone(),manifest,
                provenance:rsi_packages::Provenance::Local }];
            let deadline = std::time::Instant::now()+Duration::from_secs(5);
            loop {
                check()?;
                match store.install_checked(&source,&[],&interfaces,
                    &format!("{}-{}",std::env::consts::OS,std::env::consts::ARCH),
                    || check().map_err(|e| rsi_packages::Error::Invalid(e.into()))) {
                    Ok(_) => break,
                    Err(rsi_packages::Error::Busy) if std::time::Instant::now() < deadline =>
                        std::thread::sleep(Duration::from_millis(25)),
                    Err(rsi_packages::Error::Busy) => return Err("package writer remained busy; previous selection retained"),
                    Err(rsi_packages::Error::Digest) => return Err("source package digest differs; previous selection retained"),
                    Err(rsi_packages::Error::Io(_)) => return Err("source package filesystem admission failed; previous selection retained"),
                    Err(_) => return Err("source package contract or input fence rejected; previous selection retained"),
                }
            }
            Ok(BuildUpdate::Published { digest, restart_required:matches!(parsed.name.as_str(),"core"|"tui") })
        }).await.map_err(|_| "source publication worker failed")?
    }
}
use sha2::Digest as _;

#[cfg(all(test, windows))]
mod tests {
    use super::*;
    #[test]
    fn output_case_aliases_cannot_bypass_input_or_writer_separation() {
        assert_eq!(
            path_key(Path::new("C:\\Source\\OUT")),
            path_key(Path::new("c:\\source\\out"))
        );
        let temporary = tempfile::tempdir().unwrap();
        let recipe = BuildRecipe {
            package: "web".into(),
            directory: temporary.path().into(),
            command: vec![std::env::current_exe().unwrap().to_str().unwrap().into()],
            output: "OUT/package.json".into(),
            inputs: vec!["out".into()],
            environment: BTreeMap::new(),
        };
        assert_eq!(
            Captured::open(recipe).err(),
            Some("invalid source input/output layout")
        );
    }
}
