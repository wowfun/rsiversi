#![cfg(target_os = "linux")]

use rsi_ssh_helper::{ArtifactCache, CacheError};
use sha2::{Digest, Sha256};
use std::{
    io::{BufRead, BufReader, Write},
    os::unix::fs::{MetadataExt, PermissionsExt},
    process::Stdio,
};

const SERVICE: &str = "31313131313131313131313131313131";
fn fixture() -> (tempfile::TempDir, ArtifactCache) {
    let runtime = tempfile::Builder::new()
        .prefix("rsi-artifact-")
        .permissions(std::fs::Permissions::from_mode(0o700))
        .tempdir()
        .unwrap();
    let cache = ArtifactCache::open(runtime.path(), SERVICE)
        .expect("native test requires a local executable filesystem");
    (runtime, cache)
}
fn artifact(index: u8) -> (Vec<u8>, [u8; 32]) {
    let bytes = format!("#!/bin/sh\nprintf 'artifact-{index}\\n'\n").into_bytes();
    let digest = Sha256::digest(&bytes).into();
    (bytes, digest)
}
fn publish(cache: &ArtifactCache, index: u8) -> rsi_ssh_helper::ArtifactLease {
    let (bytes, digest) = artifact(index);
    cache.publish(digest, &mut bytes.as_slice()).unwrap()
}

#[test]
#[ignore = "requires a native Linux local executable filesystem"]
fn native_continuous_handoff_retains_live_versions_and_executes_immutable_bytes() {
    let (runtime, cache) = fixture();
    let launcher = publish(&cache, 1);
    let first = launcher.path().to_owned();
    let inode = std::fs::metadata(&first).unwrap().ino();
    let helper_cache = ArtifactCache::open(runtime.path(), SERVICE).unwrap();
    let helper = helper_cache.acquire(*launcher.digest()).unwrap();
    drop(launcher);
    drop(publish(&cache, 2));
    drop(publish(&cache, 3));
    drop(publish(&cache, 4));
    cache.collect().unwrap();
    assert!(first.exists());
    assert_eq!(std::fs::metadata(&first).unwrap().ino(), inode);
    assert_eq!(std::fs::metadata(&first).unwrap().mode() & 0o7777, 0o500);
    let output = std::process::Command::new(helper.path())
        .env_clear()
        .output()
        .unwrap();
    assert!(output.status.success());
    assert_eq!(output.stdout, b"artifact-1\n");
    assert!(cache.acquire(artifact(2).1).is_err());
    drop(helper);
    cache.collect().unwrap();
    assert!(!first.exists());
    assert!(cache.acquire(artifact(3).1).is_ok());
    assert!(cache.acquire(artifact(4).1).is_ok());
}

#[test]
#[ignore = "requires native Linux flock, shell and a local executable filesystem"]
fn native_independent_process_lease_blocks_gc_and_writer_inode_is_never_replaced() {
    let (_runtime, cache) = fixture();
    let launcher = publish(&cache, 1);
    let first = launcher.path().to_owned();
    let writer = first.parent().unwrap().join("writer.lock");
    let writer_inode = std::fs::metadata(&writer).unwrap().ino();
    let mut child = std::process::Command::new("/usr/bin/flock")
        .args(["--shared"])
        .arg(&first)
        .args(["/bin/sh", "-c", "printf ready; printf '\\n'; read ignored"])
        .env_clear()
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut ready = String::new();
    BufReader::new(child.stdout.take().unwrap())
        .read_line(&mut ready)
        .unwrap();
    assert_eq!(ready, "ready\n");
    drop(launcher);
    drop(publish(&cache, 2));
    drop(publish(&cache, 3));
    cache.collect().unwrap();
    assert!(first.exists());
    child.stdin.take().unwrap().write_all(b"done\n").unwrap();
    assert!(child.wait().unwrap().success());
    cache.collect().unwrap();
    assert!(!first.exists());
    assert_eq!(std::fs::metadata(&writer).unwrap().ino(), writer_inode);

    let locked = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&writer)
        .unwrap();
    rustix::fs::flock(
        &locked,
        rustix::fs::FlockOperation::NonBlockingLockExclusive,
    )
    .unwrap();
    assert_eq!(cache.collect(), Err(CacheError::Busy));
    assert_eq!(
        cache
            .publish(artifact(4).1, &mut artifact(4).0.as_slice())
            .unwrap_err(),
        CacheError::Busy
    );
    drop(locked);
    cache.collect().unwrap();
    assert_eq!(std::fs::metadata(&writer).unwrap().ino(), writer_inode);
}

#[test]
#[ignore = "requires a native Linux local executable filesystem"]
fn native_failed_digest_and_interrupted_metadata_reconcile_without_cross_service_collection() {
    let (runtime, cache) = fixture();
    assert_eq!(
        cache
            .publish(artifact(1).1, &mut b"bad".as_slice())
            .unwrap_err(),
        CacheError::Digest
    );
    assert!(cache.acquire(artifact(1).1).is_err());
    let first = publish(&cache, 1);
    let first_path = first.path().to_owned();
    let state = first_path.parent().unwrap().join("state.json");
    // A crash after artifact rename but before metadata publication leaves an orphan.
    std::fs::remove_file(&state).unwrap();
    cache.collect().unwrap();
    assert!(first.path().exists());
    let other = ArtifactCache::open(runtime.path(), &"4".repeat(32)).unwrap();
    let other_lease = publish(&other, 1);
    drop(first);
    drop(publish(&cache, 2));
    drop(publish(&cache, 3));
    assert!(!first_path.exists());
    assert!(other_lease.path().exists());
    assert_ne!(other_lease.path().parent(), first_path.parent());
    // A crash after an old unlink but before metadata rename leaves a missing name.
    let old = cache.acquire(artifact(2).1).unwrap();
    let missing = old.path().to_owned();
    drop(old);
    std::fs::remove_file(missing).unwrap();
    cache.collect().unwrap();
    assert!(cache.acquire(artifact(3).1).is_ok());
    std::fs::write(state, br#"{"newest":["../outside"]}"#).unwrap();
    assert_eq!(cache.collect(), Err(CacheError::Invalid));
    assert!(other_lease.path().exists());
}

#[test]
#[ignore = "requires native Linux permissions and filesystem mount flags"]
fn native_symlinks_hardlinks_unsafe_modes_and_noexec_roots_are_rejected() {
    if let Some(root) = std::env::var_os("RSI_TEST_NOEXEC_ROOT") {
        assert_eq!(
            ArtifactCache::open(std::path::Path::new(&root), SERVICE).unwrap_err(),
            CacheError::Unsafe
        );
        return;
    }
    let (runtime, cache) = fixture();
    let lease = publish(&cache, 1);
    let path = lease.path().to_owned();
    let linked = path.parent().unwrap().join("unexpected");
    std::fs::hard_link(&path, &linked).unwrap();
    assert_eq!(
        cache.acquire(*lease.digest()).unwrap_err(),
        CacheError::Unsafe
    );
    std::fs::remove_file(linked).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
    assert_eq!(
        cache.acquire(*lease.digest()).unwrap_err(),
        CacheError::Unsafe
    );
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o500)).unwrap();
    let alias = runtime.path().join("alias");
    std::os::unix::fs::symlink(runtime.path(), &alias).unwrap();
    assert_eq!(
        ArtifactCache::open(&alias, SERVICE).unwrap_err(),
        CacheError::Unsafe
    );
    std::fs::set_permissions(runtime.path(), std::fs::Permissions::from_mode(0o755)).unwrap();
    assert_eq!(
        ArtifactCache::open(runtime.path(), SERVICE).unwrap_err(),
        CacheError::Unsafe
    );
    std::fs::set_permissions(runtime.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    // Do not assume /dev/shm is noexec: this host deliberately mounts it executable.
    // Create the actual mount restriction inside a private user/mount namespace.
    let noexec = tempfile::Builder::new()
        .permissions(std::fs::Permissions::from_mode(0o700))
        .tempdir()
        .unwrap();
    let _nested = ArtifactCache::open(noexec.path(), SERVICE).unwrap();
    for mounted in [
        noexec.path().to_owned(),
        noexec.path().join("rsi-ssh").join(SERVICE),
    ] {
        let result = std::process::Command::new("/usr/bin/unshare").env_clear()
        .env("RSI_TEST_NOEXEC_ROOT", noexec.path())
        .args(["--user", "--map-root-user", "--mount", "--propagation=private", "--", "/bin/sh", "-c",
            "/usr/bin/mount -t tmpfs -o noexec,mode=0700 tmpfs \"$1\" && exec \"$2\" --exact native_symlinks_hardlinks_unsafe_modes_and_noexec_roots_are_rejected --ignored --nocapture", "rsi-noexec"])
        .arg(&mounted).arg(std::env::current_exe().unwrap()).output().unwrap();
        assert!(
            result.status.success(),
            "{}{}",
            String::from_utf8_lossy(&result.stdout),
            String::from_utf8_lossy(&result.stderr)
        );
    }
}

#[test]
#[ignore = "requires native Linux flock and a local executable filesystem"]
fn native_publication_capacity_cannot_evict_live_helpers() {
    let (_runtime, cache) = fixture();
    let live = (0..32)
        .map(|index| publish(&cache, index))
        .collect::<Vec<_>>();
    let (bytes, digest) = artifact(32);
    assert_eq!(
        cache.publish(digest, &mut bytes.as_slice()).unwrap_err(),
        CacheError::Capacity
    );
    for lease in &live {
        assert!(lease.path().exists());
    }
    drop(live);
    cache.collect().unwrap();
    assert!(cache.publish(digest, &mut bytes.as_slice()).is_ok());
}

#[test]
#[ignore = "requires a native Linux local executable filesystem"]
fn native_digest_tampering_and_replaced_directory_cannot_issue_a_lease() {
    let (runtime, cache) = fixture();
    let lease = publish(&cache, 1);
    let path = lease.path().to_owned();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
    std::fs::write(&path, b"changed bytes").unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o500)).unwrap();
    assert_eq!(
        cache.acquire(*lease.digest()).unwrap_err(),
        CacheError::Digest
    );
    let directory = path.parent().unwrap();
    let moved = runtime.path().join("old-cache");
    std::fs::rename(directory, &moved).unwrap();
    std::fs::create_dir(directory).unwrap();
    std::fs::set_permissions(directory, std::fs::Permissions::from_mode(0o700)).unwrap();
    assert_eq!(
        cache.acquire(*lease.digest()).unwrap_err(),
        CacheError::Unsafe
    );
    assert_eq!(cache.collect(), Err(CacheError::Unsafe));
    assert!(!directory.join("writer.lock").exists());
}
