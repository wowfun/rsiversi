//! Shared graceful lifecycle shutdown for launchers and offline maintenance.
use crate::{HostOwnerMode, LifecycleIdentity, LifecycleOperation, ServiceHostError};
/// Graceful drain plus the allowance for the verified native process to exit.
pub const SERVICE_HOST_STOP_TIMEOUT: std::time::Duration =
    crate::SERVICE_HOST_DRAIN_TIMEOUT.saturating_add(std::time::Duration::from_secs(15));
/// Stops exactly the observed daemon generation and waits for native exit.
pub async fn stop_gracefully(owner: &LifecycleIdentity) -> Result<(), ServiceHostError> {
    if !owner.is_current()? {
        return Ok(());
    }
    let method = if cfg!(windows) {
        "inspect this PID in Task Manager and close its owning application; end that verified process only if necessary"
    } else {
        "inspect this PID with ps -p PID -o pid,lstart,args and close its owning application or send kill -TERM PID after verifying identity"
    };
    let failure = |message: String| {
        ServiceHostError::Invalid(format!(
            "{message}; Host PID {} (start token {}). {method}, then retry; it was not force-killed",
            owner.pid, owner.process_start_token
        ))
    };
    if owner.mode != HostOwnerMode::Daemon {
        return Err(failure(
            "active Host is embedded; close its owning application first".into(),
        ));
    }
    crate::request_lifecycle_identity(owner, LifecycleOperation::Stop)
        .await
        .map_err(|e| failure(e.to_string()))?;
    let deadline = tokio::time::Instant::now() + SERVICE_HOST_STOP_TIMEOUT;
    while owner.is_current()? {
        if tokio::time::Instant::now() >= deadline {
            return Err(failure("graceful stop timed out".into()));
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    Ok(())
}

/// Cleans only the stopped generation's recorded endpoints under its home owner lease.
pub fn cleanup_stopped_endpoints(
    owner: &LifecycleIdentity,
    lease: &crate::HostOwnerLease,
) -> Result<(), ServiceHostError> {
    owner.validate()?;
    if owner.is_current()? {
        return Err(ServiceHostError::OwnerActive);
    }
    if owner.home_authority.as_deref() != Some(lease.paths().home_authority()?.as_str()) {
        return Err(ServiceHostError::Invalid(
            "runtime cleanup requires a recorded matching home authority".into(),
        ));
    }
    #[cfg(unix)]
    if let Some(path) = &owner.socket_path {
        let parent = path
            .parent()
            .ok_or_else(|| ServiceHostError::Invalid("endpoint has no parent".into()))?;
        let _directory = match rsi_files_native_fs::open_absolute_directory_no_follow(parent) {
            Ok(directory) => directory,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(crate::owner::io_error(e)),
        };
        for endpoint in [path.clone(), crate::owner::lifecycle_endpoint(path)] {
            crate::socket::remove_stale_socket_after_failed_probe(&endpoint)?;
        }
    }
    Ok(())
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    #[tokio::test]
    async fn endpoint_cleanup_requires_home_authority_and_never_scans_the_runtime_root() {
        let root = tempfile::tempdir().unwrap();
        let homes = rsi_host::HostPaths::new(
            root.path().join("config"),
            root.path().join("state"),
            root.path().join("cache"),
        )
        .unwrap();
        let paths = crate::ServiceHostPaths::from_host_paths_with_runtime(
            &homes,
            Some(&root.path().join("external")),
        )
        .unwrap();
        let _startup = crate::HostStartupLease::try_acquire(&paths).unwrap();
        let lease = crate::HostOwnerLease::try_acquire(paths.clone()).unwrap();
        std::fs::create_dir_all(paths.runtime_directory()).unwrap();
        std::fs::write(paths.runtime_directory().join("unrelated"), b"keep").unwrap();
        let listener = std::os::unix::net::UnixListener::bind(paths.socket()).unwrap();
        let mut identity = LifecycleIdentity {
            pid: i32::MAX as u32,
            process_start_token: "0".into(),
            mode: HostOwnerMode::Daemon,
            host_epoch: rsi_api_protocol::HostEpoch::generate().unwrap(),
            home_authority: None,
            socket_path: Some(paths.socket().into()),
        };
        assert!(cleanup_stopped_endpoints(&identity, &lease).is_err());
        identity.home_authority = Some(paths.home_authority().unwrap());
        assert!(cleanup_stopped_endpoints(&identity, &lease).is_err());
        drop(listener);
        cleanup_stopped_endpoints(&identity, &lease).unwrap();
        assert!(!paths.socket().exists());
        assert_eq!(
            std::fs::read(paths.runtime_directory().join("unrelated")).unwrap(),
            b"keep"
        );
    }
    #[tokio::test]
    async fn embedded_owner_failure_identifies_the_process_without_stopping_it() {
        let metadata = crate::HostOwnerMetadata::current(
            HostOwnerMode::Embedded,
            "a".repeat(64),
            rsi_api_protocol::HostEpoch::generate().unwrap(),
            rsi_api_protocol::EndpointId::from_bytes([1; 16]),
            None,
        )
        .unwrap();
        let identity = LifecycleIdentity::from(&metadata);
        let failure = stop_gracefully(&identity).await.unwrap_err().to_string();
        assert!(failure.contains(&format!("PID {}", std::process::id())));
        assert!(failure.contains("kill -TERM"));
        assert!(identity.is_current().unwrap());
    }
}

#[cfg(test)]
mod deadline_tests {
    #[test]
    fn graceful_stop_preserves_the_public_drain_and_exit_budget() {
        assert_eq!(
            super::SERVICE_HOST_STOP_TIMEOUT,
            std::time::Duration::from_secs(75)
        );
    }
}
