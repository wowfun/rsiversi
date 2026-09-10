use async_trait::async_trait;
use rsi_host::{
    Host, HostBuilder, Profile, ProfileControl, ProfileEntry, ProfileFragment, ProfileProgram,
    ProfileUpdateHandle, ReloadOutcome,
};
use rsi_meta::{
    ActivationPlan, ConfigValue, Context, LocalContract, MetaError, PluginFactory,
    PreparedActivation, ResolvedFactory, Runtime, UpdateMode,
};
use rsi_meta_profile::ProfileError;
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};
use tokio::sync::{Notify, Semaphore};

#[derive(Debug)]
struct Label;
impl LocalContract for Label {
    const KEY: &'static str = "fixture.label";
    type Service = String;
}
#[derive(Debug)]
struct OtherLabel;
impl LocalContract for OtherLabel {
    const KEY: &'static str = "fixture.label";
    type Service = String;
}
#[derive(Debug)]
struct Gate {
    entered: Notify,
    release: Semaphore,
    used: AtomicBool,
}
impl Gate {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            entered: Notify::new(),
            release: Semaphore::new(0),
            used: AtomicBool::new(false),
        })
    }
}
#[derive(Debug)]
struct Leaf {
    text: &'static str,
    calls: AtomicUsize,
    fail: AtomicBool,
    gate: Option<Arc<Gate>>,
}
impl Leaf {
    fn new(text: &'static str) -> Arc<Self> {
        Arc::new(Self {
            text,
            calls: AtomicUsize::new(0),
            fail: AtomicBool::new(false),
            gate: None,
        })
    }
}
#[async_trait]
impl PluginFactory for Leaf {
    fn prepare(&self, desired: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        Ok(PreparedActivation::new(desired.clone()))
    }
    async fn activate(&self, plan: ActivationPlan) -> rsi_meta::Result<()> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if self.fail.load(Ordering::SeqCst) {
            return Err(MetaError::Activation("fixture rejection".into()));
        }
        let label = plan
            .context()
            .provide_local::<Label>(Arc::new(self.text.into()))?;
        let gate = self.gate.clone();
        plan.defer(
            "withdraw label",
            Box::new(move || {
                Box::pin(async move {
                    drop(label);
                    if let Some(gate) = gate
                        && !gate.used.swap(true, Ordering::SeqCst)
                    {
                        gate.entered.notify_one();
                        gate.release.acquire().await.expect("gate open").forget();
                    }
                    Ok(())
                })
            }),
        )
    }
}
fn host(leaf: &Arc<Leaf>, revision: &str, mode: UpdateMode, unused: bool) -> Host {
    host_with_limits(
        leaf,
        revision,
        mode,
        unused,
        rsi_meta::RuntimeLimits::default(),
    )
}
fn host_with_limits(
    leaf: &Arc<Leaf>,
    revision: &str,
    mode: UpdateMode,
    unused: bool,
    limits: rsi_meta::RuntimeLimits,
) -> Host {
    let mut builder = HostBuilder::without_paths("native").runtime_limits(limits);
    builder.register_local_contract::<Label>().unwrap();
    builder
        .register_linked("label", revision, mode, leaf.clone())
        .unwrap();
    if unused {
        builder
            .register_linked(
                "unused",
                revision,
                UpdateMode::Replayable,
                Leaf::new("unused"),
            )
            .unwrap();
    }
    builder
        .register_fragment(ProfileFragment::new(
            "fixture",
            vec![ProfileEntry::new("leaf", "label", ConfigValue::Null)],
        ))
        .unwrap();
    builder.build().unwrap()
}
fn program() -> ProfileProgram {
    ProfileProgram::from_profile(Profile::default())
}

#[tokio::test]
async fn shutdown_releases_profile_inputs_while_child_cleanup_is_still_blocked() {
    use std::time::Duration;
    let gate = Gate::new();
    let leaf = Arc::new(Leaf {
        gate: Some(gate.clone()),
        ..Arc::try_unwrap(Leaf::new("old")).unwrap()
    });
    let limits = rsi_meta::RuntimeLimits {
        deadlines: rsi_meta::DeadlineLimits {
            shutdown_wait: Duration::from_millis(20),
            ..Default::default()
        },
        ..Default::default()
    };
    let running = host_with_limits(&leaf, "a", UpdateMode::Replayable, false, limits)
        .start_program(program())
        .await
        .unwrap();
    let outcome = running.shutdown().await;
    assert!(matches!(
        outcome,
        rsi_meta::ShutdownOutcome::TimedOut { .. }
    ));
    let health = running.profile_status().health();
    gate.release.add_permits(1);
    assert!(running.shutdown().await.is_complete());
    assert_eq!(
        health,
        rsi_host::ProfileHealth::Stopped,
        "child cleanup must not depend on executable inputs retained until the parent's cleanup effect"
    );
}

#[tokio::test]
async fn running_host_shutdown_deadline_includes_an_executing_profile_update() {
    use std::time::Duration;
    let gate = Gate::new();
    let leaf = Arc::new(Leaf {
        gate: Some(gate.clone()),
        ..Arc::try_unwrap(Leaf::new("old")).unwrap()
    });
    let limits = rsi_meta::RuntimeLimits {
        deadlines: rsi_meta::DeadlineLimits {
            shutdown_wait: Duration::from_millis(20),
            ..Default::default()
        },
        ..Default::default()
    };
    let first = host_with_limits(&leaf, "a", UpdateMode::Replayable, false, limits);
    let next = host(&Leaf::new("new"), "b", UpdateMode::Replayable, false);
    let running = first.start_program(program()).await.unwrap();
    let updater = running.updater();
    let admitted = updater
        .submit(1, next.profile_input(program()).unwrap())
        .unwrap();
    gate.entered.notified().await;
    let first_wait = tokio::time::timeout(Duration::from_secs(1), running.shutdown()).await;
    let admission_closed = matches!(
        running.reload().await,
        Err(rsi_host::HostError::Profile(ProfileError::Stopped))
    );
    gate.release.add_permits(1);
    let _ = admitted.wait().await;
    let completed = tokio::time::timeout(Duration::from_secs(2), running.shutdown())
        .await
        .unwrap();
    assert!(completed.is_complete(), "{completed:?}");
    assert!(
        matches!(first_wait, Ok(rsi_meta::ShutdownOutcome::TimedOut { .. })),
        "Host waited outside Runtime's shutdown deadline: {first_wait:?}"
    );
    assert!(admission_closed);
}

#[tokio::test]
async fn stopped_observation_handles_preserve_snapshots_without_retaining_factories() {
    let leaf = Leaf::new("retire");
    let weak = Arc::downgrade(&leaf);
    let running = Running::start(&host(&leaf, "1", UpdateMode::Replayable, false)).await;
    drop(leaf);
    let before = running.control.snapshot();
    running.updater.close().await;
    assert!(running.runtime.shutdown().await.is_clean());
    assert!(weak.upgrade().is_none());
    assert_eq!(running.control.snapshot(), before);
    assert_eq!(
        running.control.status().health(),
        rsi_host::ProfileHealth::Stopped
    );
}

struct Running {
    runtime: Runtime,
    context: Context,
    updater: ProfileUpdateHandle,
    control: Arc<dyn ProfileControl>,
}
impl Running {
    async fn start(host: &Host) -> Self {
        Self::start_program(host, program()).await
    }
    async fn start_program(host: &Host, program: ProfileProgram) -> Self {
        let runtime = Runtime::default();
        let context = host.isolate_local_context(runtime.root()).unwrap();
        let bootstrap = host.prepare_in(&runtime, program).await.unwrap();
        let updater = bootstrap.updater();
        let control = bootstrap.control();
        context
            .apply(
                ResolvedFactory::linked(
                    "profile",
                    "test",
                    UpdateMode::RestartRequired,
                    bootstrap.factory(),
                ),
                ConfigValue::Null,
            )
            .await
            .unwrap();
        Self {
            runtime,
            context,
            updater,
            control,
        }
    }
    async fn update(&self, host: &Host) -> Result<ReloadOutcome, ProfileError> {
        self.updater
            .submit(
                self.updater.input_revision(),
                host.profile_input(program()).unwrap(),
            )?
            .wait()
            .await
    }
    fn label(&self) -> Arc<String> {
        self.context.lookup_local::<Label>().unwrap()
    }
}

#[tokio::test]
async fn replacement_preserves_context_and_unused_catalog_changes_advance_only_input_revision() {
    let first = Leaf::new("first");
    let original = host(&first, "1", UpdateMode::Replayable, false);
    let running = Running::start(&original).await;
    let old_label = running.label();
    let catalog_only = host(&first, "1", UpdateMode::Replayable, true);
    assert!(matches!(
        running.update(&catalog_only).await.unwrap(),
        ReloadOutcome::Unchanged(_)
    ));
    assert_eq!(running.updater.input_revision(), 2);
    assert_eq!(running.control.status().revision(), 1);
    assert!(Arc::ptr_eq(&old_label, &running.label()));
    assert_eq!(first.calls.load(Ordering::SeqCst), 1);
    let next = host(&Leaf::new("second"), "2", UpdateMode::Replayable, true);
    assert!(matches!(
        running.update(&next).await.unwrap(),
        ReloadOutcome::Applied(_)
    ));
    assert_eq!(running.label().as_str(), "second");
    assert_eq!(running.updater.input_revision(), 3);
    assert!(matches!(
        running
            .updater
            .submit(1, original.profile_input(program()).unwrap())
            .unwrap()
            .wait()
            .await,
        Err(ProfileError::InputConflict {
            expected: 1,
            current: 3
        })
    ));
    // A new input neither mutates the old Host nor reallocates the running Local mapping.
    let independent = Running::start(&original).await;
    assert_eq!(independent.label().as_str(), "first");
    assert_eq!(old_label.as_str(), "first");
    assert!(independent.runtime.shutdown().await.is_clean());
    assert!(running.runtime.shutdown().await.is_clean());
}

#[tokio::test]
async fn compensation_uses_the_old_factory_and_does_not_commit_the_failed_input() {
    let first = Leaf::new("original");
    let running = Running::start(&host(&first, "1", UpdateMode::Replayable, false)).await;
    let failed = Leaf::new("candidate");
    failed.fail.store(true, Ordering::SeqCst);
    let candidate = host(&failed, "2", UpdateMode::Replayable, false);
    assert!(matches!(
        running.update(&candidate).await.unwrap(),
        ReloadOutcome::RolledBack { .. }
    ));
    assert_eq!(running.label().as_str(), "original");
    assert_eq!(first.calls.load(Ordering::SeqCst), 2);
    assert_eq!(failed.calls.load(Ordering::SeqCst), 1);
    assert_eq!(running.updater.input_revision(), 1);
    assert!(matches!(
        running.control.reload().await.unwrap(),
        ReloadOutcome::Unchanged(_)
    ));
    assert!(running.runtime.shutdown().await.is_clean());
}

#[tokio::test]
async fn incompatible_markers_and_restart_required_reject_input_replacement() {
    let first = Leaf::new("original");
    let running = Running::start(&host(&first, "1", UpdateMode::RestartRequired, false)).await;
    let old = running.label();
    let mut wrong = HostBuilder::without_paths("native");
    wrong.register_local_contract::<OtherLabel>().unwrap();
    assert!(matches!(
        running.update(&wrong.build().unwrap()).await,
        Err(ProfileError::IncompatibleInput(_))
    ));
    let different_environment = HostBuilder::without_paths("different").build().unwrap();
    assert!(matches!(
        running.update(&different_environment).await,
        Err(ProfileError::IncompatibleInput(_))
    ));
    let changed = host(&Leaf::new("new"), "2", UpdateMode::RestartRequired, false);
    assert!(matches!(
        running.update(&changed).await.unwrap(),
        ReloadOutcome::RestartRequired(_)
    ));
    assert!(Arc::ptr_eq(&old, &running.label()));
    assert_eq!(running.updater.input_revision(), 1);
    assert!(running.runtime.shutdown().await.is_clean());
}

#[tokio::test]
async fn refused_input_preserves_committed_target_and_restart_status_across_reload() {
    let first = Leaf::new("original");
    let original = host(&first, "1", UpdateMode::RestartRequired, false);
    let running = Running::start(&original).await;
    let before = running.control.snapshot();
    let mut builder = HostBuilder::without_paths("native");
    builder.register_local_contract::<Label>().unwrap();
    builder
        .register_linked("label", "2", UpdateMode::RestartRequired, Leaf::new("new"))
        .unwrap();
    builder
        .register_fragment(ProfileFragment::new(
            "candidate",
            vec![ProfileEntry::new("other-leaf", "label", ConfigValue::Null)],
        ))
        .unwrap();
    assert!(matches!(
        running.update(&builder.build().unwrap()).await.unwrap(),
        ReloadOutcome::RestartRequired(_)
    ));
    assert_eq!(
        running.control.snapshot(),
        before,
        "refused input must not publish its tree or digest"
    );
    for _ in 0..3 {
        let outcome = running.control.reload().await.unwrap();
        assert!(matches!(outcome, ReloadOutcome::RestartRequired(_)));
        assert_eq!(outcome.status().revision(), before.revision());
        assert_eq!(running.control.snapshot(), before);
        assert_eq!(running.updater.input_revision(), 1);
    }
    assert_eq!(first.calls.load(Ordering::SeqCst), 1);
    assert!(matches!(
        running.update(&original).await.unwrap(),
        ReloadOutcome::Unchanged(_)
    ));
    assert_eq!(
        running.control.status().health(),
        rsi_host::ProfileHealth::Converged
    );
    assert_eq!(running.control.snapshot(), before);
    assert_eq!(running.updater.input_revision(), 2);
    assert!(running.runtime.shutdown().await.is_clean());
}

#[tokio::test]
async fn dropped_ticket_finishes_convergence_and_admission_is_bounded() {
    let gate = Gate::new();
    let first = Arc::new(Leaf {
        text: "first",
        calls: AtomicUsize::new(0),
        fail: AtomicBool::new(false),
        gate: Some(gate.clone()),
    });
    let running = Running::start(&host(&first, "1", UpdateMode::Replayable, false)).await;
    let next = host(&Leaf::new("second"), "2", UpdateMode::Replayable, false);
    let ticket = running
        .updater
        .submit(1, next.profile_input(program()).unwrap())
        .unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(5), gate.entered.notified())
        .await
        .unwrap();
    drop(ticket);
    let last = host(&Leaf::new("third"), "3", UpdateMode::Replayable, false);
    let queued = running
        .updater
        .submit(2, last.profile_input(program()).unwrap())
        .unwrap();
    assert!(matches!(
        running
            .updater
            .submit(2, last.profile_input(program()).unwrap()),
        Err(ProfileError::Busy)
    ));
    gate.release.add_permits(1);
    assert!(matches!(
        queued.wait().await.unwrap(),
        ReloadOutcome::Applied(_)
    ));
    assert_eq!(running.label().as_str(), "third");
    assert_eq!(running.updater.input_revision(), 3);
    assert!(running.runtime.shutdown().await.is_clean());
    assert!(matches!(
        running
            .updater
            .submit(3, next.profile_input(program()).unwrap()),
        Err(ProfileError::Stopped)
    ));
}

#[tokio::test]
async fn closing_input_admission_joins_the_current_command_and_rejects_the_queued_input() {
    use std::{future::Future as _, task::Poll};
    let gate = Gate::new();
    let first = Arc::new(Leaf {
        text: "first",
        calls: AtomicUsize::new(0),
        fail: AtomicBool::new(false),
        gate: Some(gate.clone()),
    });
    let running = Running::start(&host(&first, "1", UpdateMode::Replayable, false)).await;
    let next = host(&Leaf::new("second"), "2", UpdateMode::Replayable, false);
    let admitted = running
        .updater
        .submit(1, next.profile_input(program()).unwrap())
        .unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(5), gate.entered.notified())
        .await
        .unwrap();
    let queued = running
        .updater
        .submit(2, next.profile_input(program()).unwrap())
        .unwrap();
    let mut closing = std::pin::pin!(running.updater.close());
    std::future::poll_fn(|cx| {
        assert!(closing.as_mut().poll(cx).is_pending());
        Poll::Ready(())
    })
    .await;
    assert!(matches!(
        running.control.reload().await,
        Err(ProfileError::Stopped)
    ));
    gate.release.add_permits(1);
    closing.await;
    assert!(matches!(
        admitted.wait().await.unwrap(),
        ReloadOutcome::Applied(_)
    ));
    assert!(matches!(queued.wait().await, Err(ProfileError::Stopped)));
    assert_eq!(running.updater.input_revision(), 2);
    assert_eq!(
        running.control.status().health(),
        rsi_host::ProfileHealth::Stopped
    );
    assert!(running.runtime.shutdown().await.is_clean());
}

#[tokio::test]
async fn dropping_manual_reload_waiter_during_cleanup_does_not_abandon_convergence() {
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("profile.toml");
    let document = |value| {
        format!(
            "format = 1\n[[steps]]\nkind = 'plugin'\nid = 'leaf'\nplugin = 'label'\nconfig = {value}\n"
        )
    };
    std::fs::write(&source, document(1)).unwrap();
    let gate = Gate::new();
    let leaf = Arc::new(Leaf {
        text: "original",
        calls: AtomicUsize::new(0),
        fail: AtomicBool::new(false),
        gate: Some(gate.clone()),
    });
    let mut builder = HostBuilder::without_paths("native");
    builder.register_local_contract::<Label>().unwrap();
    builder
        .register_linked("label", "1", UpdateMode::Replayable, leaf.clone())
        .unwrap();
    let running = Running::start_program(
        &builder.build().unwrap(),
        ProfileProgram::from_file(&source),
    )
    .await;
    std::fs::write(&source, document(2)).unwrap();
    let control = running.control.clone();
    let waiter = tokio::spawn(async move { control.reload().await });
    tokio::time::timeout(std::time::Duration::from_secs(5), gate.entered.notified())
        .await
        .unwrap();
    waiter.abort();
    assert!(waiter.await.unwrap_err().is_cancelled());
    gate.release.add_permits(1);
    let mut status = running.control.subscribe();
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            if status.borrow_and_update().revision() == 2 {
                break;
            }
            status.changed().await.unwrap();
        }
    })
    .await
    .unwrap();
    assert_eq!(
        running.control.status().health(),
        rsi_host::ProfileHealth::Converged
    );
    assert_eq!(leaf.calls.load(Ordering::SeqCst), 2);
    assert_eq!(running.updater.input_revision(), 1);
    running.updater.close().await;
    assert!(running.runtime.shutdown().await.is_clean());
}

#[derive(Debug, Default)]
struct Sidecar {
    fail: AtomicBool,
}
#[async_trait]
impl PluginFactory for Sidecar {
    fn prepare(&self, desired: &ConfigValue) -> rsi_meta::Result<PreparedActivation> {
        Ok(PreparedActivation::new(desired.clone()))
    }
    async fn activate(&self, _: ActivationPlan) -> rsi_meta::Result<()> {
        if self.fail.load(Ordering::SeqCst) {
            Err(MetaError::Activation("sidecar fixture failure".into()))
        } else {
            Ok(())
        }
    }
}
fn mixed_host(
    first: Arc<Leaf>,
    revision: &str,
    sidecar: Arc<Sidecar>,
    sidecar_revision: &str,
) -> Host {
    let mut builder = HostBuilder::without_paths("native");
    builder.register_local_contract::<Label>().unwrap();
    builder
        .register_linked("label", revision, UpdateMode::RestartRequired, first)
        .unwrap();
    builder
        .register_linked("sidecar", sidecar_revision, UpdateMode::Replayable, sidecar)
        .unwrap();
    builder
        .register_fragment(ProfileFragment::new(
            "mixed",
            vec![
                ProfileEntry::new("leaf", "label", ConfigValue::Null),
                ProfileEntry::new("sidecar", "sidecar", ConfigValue::Null),
            ],
        ))
        .unwrap();
    builder.build().unwrap()
}
#[tokio::test]
async fn rollback_and_degraded_repair_preserve_the_unacknowledged_restart_input() {
    for fail_compensation in [false, true] {
        let first = Leaf::new("original");
        let sidecar = Arc::new(Sidecar::default());
        let original = mixed_host(first.clone(), "1", sidecar.clone(), "1");
        let running = Running::start(&original).await;
        let digest = running.control.snapshot().source_digest().to_owned();
        assert!(matches!(
            running
                .update(&mixed_host(first.clone(), "2", sidecar.clone(), "1"))
                .await
                .unwrap(),
            ReloadOutcome::RestartRequired(_)
        ));
        sidecar.fail.store(fail_compensation, Ordering::SeqCst);
        let bad = Arc::new(Sidecar::default());
        bad.fail.store(true, Ordering::SeqCst);
        let result = running
            .update(&mixed_host(first, "1", bad, "2"))
            .await
            .unwrap();
        if fail_compensation {
            assert!(matches!(result, ReloadOutcome::Degraded { .. }));
            sidecar.fail.store(false, Ordering::SeqCst);
            assert!(matches!(
                running.control.reload().await.unwrap(),
                ReloadOutcome::Applied(_)
            ));
        } else {
            assert!(matches!(result, ReloadOutcome::RolledBack { .. }));
        }
        assert_eq!(
            running.control.status().health(),
            rsi_host::ProfileHealth::RestartRequired
        );
        assert_eq!(running.control.snapshot().source_digest(), digest);
        assert_eq!(running.updater.input_revision(), 1);
        assert!(matches!(
            running.control.reload().await.unwrap(),
            ReloadOutcome::RestartRequired(_)
        ));
        assert!(matches!(
            running.update(&original).await.unwrap(),
            ReloadOutcome::Unchanged(_)
        ));
        assert_eq!(
            running.control.status().health(),
            rsi_host::ProfileHealth::Converged
        );
        assert!(running.runtime.shutdown().await.is_clean());
    }
}
