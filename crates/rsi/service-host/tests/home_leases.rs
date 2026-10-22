#![cfg(any(unix, windows))]

use rsi_host::HostPaths;
use rsi_service_host::{HostOwnerLease, ServiceHostError, ServiceHostPaths};
use std::path::Path;

fn paths(root: &Path, config: &str, state: &str) -> ServiceHostPaths {
    ServiceHostPaths::from_host_paths(
        &HostPaths::new(root.join(config), root.join(state), root.join("cache")).unwrap(),
    )
    .unwrap()
}

#[test]
fn ownership_fences_either_shared_home_and_releases_partial_acquisitions() {
    let root = tempfile::tempdir().unwrap();
    let first = HostOwnerLease::try_acquire(paths(root.path(), "b-config", "c-state")).unwrap();
    for pair in [
        ("b-config", "d-state"),
        ("a-config", "c-state"),
        ("c-state", "b-config"),
    ] {
        assert_eq!(
            HostOwnerLease::try_acquire(paths(root.path(), pair.0, pair.1)).unwrap_err(),
            ServiceHostError::OwnerActive,
        );
    }
    // The earlier lock in a failed two-home acquisition must have been released.
    let independent =
        HostOwnerLease::try_acquire(paths(root.path(), "a-config", "d-state")).unwrap();
    drop(independent);
    drop(first);
    HostOwnerLease::try_acquire(paths(root.path(), "b-config", "c-state")).unwrap();
}

#[test]
fn identical_homes_use_one_authority() {
    let root = tempfile::tempdir().unwrap();
    let first = HostOwnerLease::try_acquire(paths(root.path(), "shared", "shared")).unwrap();
    assert_eq!(
        HostOwnerLease::try_acquire(paths(root.path(), "shared", "elsewhere")).unwrap_err(),
        ServiceHostError::OwnerActive,
    );
    drop(first);
    HostOwnerLease::try_acquire(paths(root.path(), "shared", "shared")).unwrap();
}

#[test]
fn readers_cannot_attach_to_the_same_state_with_a_different_config_home() {
    use rsi_api_protocol::{EndpointId, HostEpoch};
    use rsi_service_host::{HostOwnerMetadata, HostOwnerMode};
    let root = tempfile::tempdir().unwrap();
    let selected = paths(root.path(), "config", "state");
    let lease = HostOwnerLease::try_acquire(selected.clone()).unwrap();
    let metadata = HostOwnerMetadata::current(
        HostOwnerMode::Embedded,
        "a".repeat(64),
        HostEpoch::generate().unwrap(),
        EndpointId::from_bytes([2; 16]),
        None,
    )
    .unwrap();
    lease.publish(&metadata).unwrap();
    assert_eq!(
        selected.read_metadata().unwrap().unwrap().home_authority,
        Some(selected.home_authority().unwrap())
    );
    let other = paths(root.path(), "other-config", "state");
    std::fs::create_dir(root.path().join("other-config")).unwrap();
    assert!(matches!(
        other.read_metadata(),
        Err(ServiceHostError::Invalid(_))
    ));
}
