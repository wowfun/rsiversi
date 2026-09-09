use super::{Command, ManagementOutput, Operation, RsiError, boot};
use rsi::{
    NativeAddonBuild, NativeAddonBuildManager, NativeAddonBuildReport, NativeAddonBuildStatus,
    NativeAddonError, NativeAddonReceipt, NativeAddonRecord, NativeAddonStore,
};
use serde_json::{Value, json};
use std::{
    ffi::OsString,
    sync::{
        Arc,
        atomic::{AtomicU8, Ordering},
    },
    time::Duration,
};
use tokio_util::sync::CancellationToken;

const MAXIMUM_REPORT_BYTES: usize = 512 * 1024;

pub(super) async fn run(command: Command) -> u8 {
    let stop = CancellationToken::new();
    let signal_exit = Arc::new(AtomicU8::new(0));
    let signals = match signals(stop.clone(), signal_exit.clone()) {
        Ok(task) => task,
        Err(error) => return super::report_error(&error),
    };
    let result = drive(command, &stop).await;
    stop.cancel();
    let _ = signals.await;
    let exit = signal_exit.load(Ordering::Acquire);
    match result {
        Ok(code) => {
            if exit == 0 {
                code
            } else {
                exit
            }
        }
        Err(error) => {
            let reported = super::report_error(&error);
            if exit == 0 { reported } else { exit }
        }
    }
}
fn signals(
    stop: CancellationToken,
    exit: Arc<AtomicU8>,
) -> rsi::Result<tokio::task::JoinHandle<()>> {
    use tokio::signal::unix::{SignalKind, signal};
    let mut interrupt = signal(SignalKind::interrupt()).map_err(boot)?;
    let mut terminate = signal(SignalKind::terminate()).map_err(boot)?;
    let mut hangup = signal(SignalKind::hangup()).map_err(boot)?;
    Ok(tokio::spawn(async move {
        let code = tokio::select! {
            () = stop.cancelled() => return,
            _ = interrupt.recv() => 130,
            _ = terminate.recv() => 143,
            _ = hangup.recv() => 129,
        };
        exit.store(code, Ordering::Release);
        stop.cancel();
    }))
}
async fn drive(command: Command, stop: &CancellationToken) -> rsi::Result<u8> {
    let (path, watch) = match command.operation {
        Operation::Build(path) => (path, false),
        Operation::Watch(path) => (path, true),
        _ => unreachable!("build dispatch"),
    };
    let path = std::path::absolute(path).map_err(boot)?;
    let anchor = blocking(move || {
        let build = NativeAddonBuild::open(&path).map_err(boot)?;
        build.fingerprint().map_err(boot)?;
        if watch && !build.has_watch_inputs() {
            return Err(RsiError::Boot(
                "watch requires explicit build.watch inputs".into(),
            ));
        }
        Ok(Arc::new(build))
    })
    .await?;
    if stop.is_cancelled() {
        return Ok(130);
    }
    let paths = super::super::standard_paths()?;
    let root = command
        .root
        .unwrap_or_else(|| paths.config().join("native-addons"));
    let store = blocking(move || {
        let root = rsi_files_native_fs::resolve_absolute_root_alias(&root, true).map_err(boot)?;
        NativeAddonStore::open(root).map(Arc::new).map_err(boot)
    })
    .await?;
    let writer = rsi_terminal::ManagementWriter::new(MAXIMUM_REPORT_BYTES, stop)?;
    let manager = NativeAddonBuildManager::open(paths).await?;
    let result = watch_or_build(
        &manager,
        &writer,
        anchor,
        store,
        LoopOptions {
            enable: command.enable,
            watch,
            output: command.output,
        },
        stop,
    )
    .await;
    writer.close().await;
    let outcome = manager.shutdown().await;
    if !outcome.is_clean() {
        return Err(RsiError::Run(format!(
            "native build cleanup failed: {outcome:?}"
        )));
    }
    result
}
async fn blocking<T: Send + 'static>(
    work: impl FnOnce() -> rsi::Result<T> + Send + 'static,
) -> rsi::Result<T> {
    tokio::task::spawn_blocking(work)
        .await
        .map_err(|_| RsiError::Run("native build filesystem worker failed".into()))?
}
struct Observation {
    build: Arc<NativeAddonBuild>,
    fingerprint: String,
}
async fn observe(
    anchor: Arc<NativeAddonBuild>,
) -> rsi::Result<std::result::Result<Observation, (bool, NativeAddonError)>> {
    blocking(move || {
        let result = (|| {
            let build = anchor
                .reopen()
                .map_err(|error| (matches!(error, NativeAddonError::Conflict), error))?;
            if build.id() != anchor.id() {
                return Err((
                    true,
                    NativeAddonError::Invalid("watched addon identity changed"),
                ));
            }
            let fingerprint = build.fingerprint().map_err(|error| (false, error))?;
            Ok(Observation {
                build: Arc::new(build),
                fingerprint,
            })
        })();
        Ok(result)
    })
    .await
}
async fn selected(
    store: Arc<NativeAddonStore>,
    id: String,
) -> rsi::Result<Option<NativeAddonRecord>> {
    blocking(move || {
        Ok(store
            .snapshot()
            .map_err(boot)?
            .enabled
            .into_iter()
            .find(|record| record.id() == id))
    })
    .await
}
struct LoopOptions {
    enable: bool,
    watch: bool,
    output: ManagementOutput,
}
async fn watch_or_build(
    manager: &NativeAddonBuildManager,
    writer: &rsi_terminal::ManagementWriter,
    anchor: Arc<NativeAddonBuild>,
    store: Arc<NativeAddonStore>,
    options: LoopOptions,
    stop: &CancellationToken,
) -> rsi::Result<u8> {
    let mut expected = selected(store.clone(), anchor.id().to_owned()).await?;
    let environment: Vec<(OsString, OsString)> = std::env::vars_os().collect();
    let mut previous = None;
    let mut last_error = None;
    let mut ticks = tokio::time::interval(Duration::from_secs(1));
    ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tokio::select! { biased; () = stop.cancelled() => return Ok(130), _ = ticks.tick() => {} }
        let current = selected(store.clone(), anchor.id().to_owned()).await?;
        if options.enable && current != expected {
            return Err(RsiError::Run(
                "enabled selection changed outside this build; watch stopped".into(),
            ));
        }
        let observed = match observe(anchor.clone()).await? {
            Ok(value) => value,
            Err((fatal, error)) => {
                if !options.watch || fatal {
                    return Err(boot(error));
                }
                let error = error.to_string();
                if last_error.as_ref() != Some(&error) {
                    notice(writer, options.output, &error).await?;
                    last_error = Some(error);
                }
                continue;
            }
        };
        if options.watch && !observed.build.has_watch_inputs() {
            return Err(RsiError::Run(
                "watch declaration no longer has explicit inputs".into(),
            ));
        }
        last_error = None;
        if previous.as_ref() == Some(&observed.fingerprint) {
            continue;
        }
        let report = match manager
            .service()
            .run(
                observed.build,
                store.clone(),
                environment.clone(),
                stop.clone(),
            )
            .await
        {
            Ok(report) => report,
            Err(rsi::NativeAddonBuildError::Source(NativeAddonError::Conflict))
                if options.watch =>
            {
                previous = None;
                notice(
                    writer,
                    options.output,
                    "build inputs changed before publication; waiting for current inputs",
                )
                .await?;
                continue;
            }
            Err(error) => return Err(RsiError::Run(error.to_string())),
        };
        previous = Some(report.source_fingerprint.clone());
        let (enabled, enable_error) = if options.enable
            && report.status == NativeAddonBuildStatus::Succeeded
            && !stop.is_cancelled()
        {
            enable_report(&report, &store, &mut expected).await?
        } else {
            (None, None)
        };
        let exit =
            u8::from(report.status != NativeAddonBuildStatus::Succeeded || enable_error.is_some());
        writer
            .write(render(
                &report,
                enabled.as_ref(),
                enable_error.as_deref(),
                options.output,
            )?)
            .await?;
        if !options.watch || enable_error.is_some() {
            return Ok(exit);
        }
    }
}
async fn enable_report(
    report: &NativeAddonBuildReport,
    store: &Arc<NativeAddonStore>,
    expected: &mut Option<NativeAddonRecord>,
) -> rsi::Result<(Option<NativeAddonReceipt>, Option<String>)> {
    let record = report
        .installed
        .as_ref()
        .and_then(|receipt| receipt.record.as_ref())
        .ok_or_else(|| RsiError::Run("build did not return an installed record".into()))?
        .clone();
    let selected = record.clone();
    let before = expected.clone();
    let source = store.clone();
    let result =
        tokio::task::spawn_blocking(move || source.enable_exact(&selected, before.as_ref()))
            .await
            .map_err(|_| RsiError::Run("conditional enable worker failed".into()))?;
    Ok(match result {
        Ok(receipt) => {
            *expected = Some(record);
            (Some(receipt), None)
        }
        Err(error) => (None, Some(error.to_string())),
    })
}
fn receipt(value: &NativeAddonReceipt) -> Value {
    json!({"revision":value.revision.to_string(),"changed":value.changed,"directory_synced":value.directory_synced,"record":value.record})
}
fn render(
    report: &NativeAddonBuildReport,
    enabled: Option<&NativeAddonReceipt>,
    enable_error: Option<&str>,
    output: ManagementOutput,
) -> rsi::Result<Vec<u8>> {
    let stream = |value: &rsi_process::ProcessRead| json!({"bytes_hex":hex::encode(&value.bytes),"oldest_offset":value.oldest_offset.to_string(),"next_offset":value.next_offset.to_string(),"lossy":value.lossy});
    let value = json!({
        "version":1,"kind":"native_addon_build","status":report.status,"source_fingerprint":report.source_fingerprint,
        "exit_code":report.exit_code,"signal":report.signal,"installed":report.installed.as_ref().map(receipt),
        "enabled":enabled.map(receipt),"enable_error":enable_error,
        "stdout":stream(&report.stdout),"stderr":stream(&report.stderr),
        "enforcement":{"requested":report.enforcement.requested,"backend":report.enforcement.backend,"filesystem":report.enforcement.filesystem,"scratch":report.enforcement.scratch,"network":report.enforcement.network}
    });
    match output {
        ManagementOutput::Json => serde_json::to_vec(&value).map_err(boot),
        ManagementOutput::Text => {
            let mut summary = value;
            summary
                .as_object_mut()
                .expect("build report object")
                .remove("stdout");
            summary
                .as_object_mut()
                .expect("build report object")
                .remove("stderr");
            let mut text = serde_json::to_string_pretty(&summary).map_err(boot)?;
            for (name, tail) in [("stdout", &report.stdout), ("stderr", &report.stderr)] {
                if !tail.bytes.is_empty() {
                    use std::fmt::Write as _;
                    write!(
                        text,
                        "\n{name} [{}..{}, lossy={}]:\n{}",
                        tail.oldest_offset,
                        tail.next_offset,
                        tail.lossy,
                        rsi_terminal::terminal_text(&String::from_utf8_lossy(&tail.bytes))
                    )
                    .expect("String writes cannot fail");
                }
            }
            Ok(text.into_bytes())
        }
    }
}
async fn notice(
    writer: &rsi_terminal::ManagementWriter,
    output: ManagementOutput,
    error: &str,
) -> rsi::Result<()> {
    let bytes = match output {
        ManagementOutput::Json => serde_json::to_vec(
            &json!({"version":1,"kind":"native_addon_watch_error","error":error}),
        )
        .map_err(boot)?,
        ManagementOutput::Text => {
            format!("watch: {}", rsi_terminal::terminal_text(error)).into_bytes()
        }
    };
    writer.write(bytes).await
}
