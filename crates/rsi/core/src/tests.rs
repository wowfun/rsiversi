#[cfg(target_os = "linux")]
use super::host_cli::{
    DaemonControlEvent, format_service_host_diagnostics, host_stop_timeout,
    next_daemon_control_event, stop_reload_task,
};
use super::*;

#[cfg(target_os = "linux")]
#[tokio::test]
async fn daemon_join_failure_drains_reload_diagnostics_and_presets_before_returning() {
    let daemon = tokio::spawn(async { panic!("injected daemon failure") });
    let result = daemon.await;
    assert!(result.is_err());
    let reload_done = CancellationToken::new();
    let guard = reload_done.clone().drop_guard();
    let mut reload = Some(tokio::spawn(async move {
        let _guard = guard;
        std::future::pending::<()>().await;
    }));
    let diagnostics_stop = CancellationToken::new();
    let diagnostics_done = CancellationToken::new();
    let stopped = diagnostics_done.clone();
    let stop = diagnostics_stop.clone();
    let diagnostics = tokio::spawn(async move {
        stop.cancelled().await;
        stopped.cancel();
    });
    let presets_done = CancellationToken::new();
    let finalizing = super::host_cli::finish_daemon_shutdown(
        result,
        &mut reload,
        diagnostics_stop,
        diagnostics,
        async {
            assert!(reload_done.is_cancelled() && diagnostics_done.is_cancelled());
            presets_done.cancel();
            Ok(())
        },
    );
    let error = tokio::time::timeout(Duration::from_secs(1), finalizing)
        .await
        .unwrap()
        .unwrap_err();
    assert!(error.to_string().contains("Service Host task failed"));
    assert!(reload.is_none() && presets_done.is_cancelled());
}

#[cfg(target_os = "linux")]
#[test]
fn service_host_diagnostics_format_has_stable_explicit_fields() {
    let formatted = format_service_host_diagnostics(
        ServiceHostDiagnosticsSnapshot {
            accepted_connections: 2,
            api: rsi_api_http::HttpDiagnosticsSnapshot {
                rejected_requests: 1,
                ..Default::default()
            },
            ..ServiceHostDiagnosticsSnapshot::default()
        },
        true,
    );

    assert_eq!(
        formatted,
        "Service Host diagnostics final=true accepted_connections=2 accept_errors=0 peer_credential_errors=0 foreign_uid_rejections=0 capacity_rejections=0 service_failures=0 api_rejected_requests=1 api_failed_requests=0 api_connection_failures=0 api_tls_failures=0 connection_task_panics=0 drain_aborted_connections=0"
    );
}

fn parse(arguments: &[&str]) -> rsi::Result<Parse> {
    cli::parse_cli(arguments.iter().map(OsString::from))
}

#[test]
fn agent_store_verify_has_a_strict_absolute_root_contract() {
    let Parse::AgentStore(command) = parse(&[
        "agent-store",
        "verify",
        "--root",
        "/tmp/rsi-agent-store",
        "--output",
        "json",
    ])
    .unwrap() else {
        panic!("agent-store")
    };
    assert_eq!(command.root, Some(PathBuf::from("/tmp/rsi-agent-store")));
    assert_eq!(command.output, ManagementOutput::Json);
    assert!(parse(&["agent-store", "verify", "--root", "relative"]).is_err());
    assert!(parse(&["agent-store", "verify", "--root", "/a", "--root", "/b"]).is_err());
    assert!(parse(&["agent-store", "unknown"]).is_err());
}

#[test]
fn parses_named_applications_and_strict_profile_management() {
    let Parse::Application(application) = parse(&[
        "--profile",
        "headless",
        "task",
        "--session-id",
        "session-one",
    ])
    .unwrap() else {
        panic!("application")
    };
    assert_eq!(application.profile.as_str(), "headless");
    assert_eq!(application.arguments.len(), 3);

    let Parse::Profile(profile) = parse(&[
        "profile", "host", "copy", "standard", "custom", "--output", "json",
    ])
    .unwrap() else {
        panic!("profile")
    };
    assert_eq!(profile.kind, ProfileKind::Host);
    assert_eq!(profile.operation, ProfileOperationKind::Copy);
    assert_eq!(profile.ids, ["standard", "custom"]);
    assert_eq!(profile.output, ManagementOutput::Json);
    assert!(parse(&["profile", "application", "preview", "session"]).is_err());
    assert!(parse(&["profile", "host", "delete"]).is_err());
}

#[test]
#[cfg(target_os = "linux")]
fn parses_explicit_host_lifecycle_without_ambiguous_targets() {
    let Parse::Host(command) =
        parse(&["host", "restart", "--profile", "custom", "--force"]).unwrap()
    else {
        panic!("host")
    };
    assert_eq!(command.operation, HostOperation::Restart);
    assert_eq!(command.profile.as_str(), "custom");
    assert!(command.force);
    assert!(parse(&["host", "status", "--profile", "custom"]).is_err());
    assert!(parse(&["host", "reload", "--force"]).is_err());

    let Parse::Host(detached) = parse(&["host", "serve", "--detached-child"]).unwrap() else {
        panic!("detached serve")
    };
    assert!(detached.detached_child);
    assert!(parse(&["host", "start", "--detached-child"]).is_err());
}

#[test]
#[cfg(not(target_os = "linux"))]
fn host_commands_validate_syntax_but_have_no_daemon_state_on_this_platform() {
    assert!(matches!(
        parse(&["host", "start"]).unwrap(),
        Parse::HostUnsupported
    ));
    assert!(matches!(
        parse(&["host", "--help"]).unwrap(),
        Parse::Help(_)
    ));
    assert!(parse(&["host", "reload", "--force"]).is_err());
}

#[cfg(target_os = "linux")]
#[test]
fn graceful_host_stop_wait_includes_drain_and_shutdown_margin() {
    assert_eq!(
        host_stop_timeout(false),
        SERVICE_HOST_DRAIN_TIMEOUT + HOST_SHUTDOWN_MARGIN
    );
    assert!(host_stop_timeout(false) > SERVICE_HOST_DRAIN_TIMEOUT);
    assert_eq!(host_stop_timeout(true), FORCE_HOST_STOP_TIMEOUT);
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn daemon_stop_selection_remains_live_during_reload() {
    let mut daemon_task = tokio::spawn(std::future::pending::<rsi::Result<()>>());
    let mut reload_task = tokio::spawn(std::future::pending::<()>());

    let event = next_daemon_control_event(
        &mut daemon_task,
        std::future::ready(Some(())),
        std::future::pending::<Option<()>>(),
        std::future::pending::<Option<()>>(),
        false,
        Some(&mut reload_task),
    )
    .await;

    assert!(matches!(event, DaemonControlEvent::Stop));
    assert!(!reload_task.is_finished());
    let mut reload_task = Some(reload_task);
    stop_reload_task(&mut reload_task).await;
    assert!(reload_task.is_none());
    // Once TERM starts shutdown, a ready SIGHUP must not start another reload.
    daemon_task.abort();
    let event = next_daemon_control_event(
        &mut daemon_task,
        std::future::pending::<Option<()>>(),
        std::future::pending::<Option<()>>(),
        std::future::ready(Some(())),
        false,
        None,
    )
    .await;
    assert!(matches!(event, DaemonControlEvent::Daemon(_)));
}

#[test]
fn removed_direct_run_is_rejected() {
    assert!(parse(&["run", "task"]).is_err());
}
