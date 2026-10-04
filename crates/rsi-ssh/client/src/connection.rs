use crate::{PreparedSsh, ProcessConnection, Result, SshClientError};
use rsi_ssh_protocol::initialization::Initialization;
use rsi_ssh_transport::{Connection, Role};
use sha2::{Digest, Sha256};
use std::{io::Read, path::Path, process::Stdio, sync::Arc, time::Duration};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio_util::sync::CancellationToken;

const MAXIMUM_IMAGE_BYTES: u64 = 128 * 1024 * 1024;
const CONNECT_DEADLINE: Duration = Duration::from_secs(45);
const STARTUP_REAP_DEADLINE: Duration = Duration::from_secs(5);
type Reaped = Option<std::result::Result<std::process::ExitStatus, ()>>;

/// Verified same-CPU Linux image whose bytes cannot change during deployment.
#[derive(Clone)]
pub struct HelperArtifact {
    bytes: Arc<[u8]>,
    digest: [u8; 32],
}
impl std::fmt::Debug for HelperArtifact {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HelperArtifact")
            .field("bytes", &self.bytes.len())
            .field("digest", &hex::encode(self.digest))
            .finish()
    }
}
impl HelperArtifact {
    /// Checks bounded bytes against the distribution owner's expected digest.
    pub async fn load(path: &Path, digest: [u8; 32]) -> Result<Self> {
        let path = path.to_path_buf();
        tokio::task::spawn_blocking(move || {
            let file = std::fs::File::open(path).map_err(|_| SshClientError::Io)?;
            let metadata = file.metadata().map_err(|_| SshClientError::Io)?;
            if !metadata.is_file() || !(64..=MAXIMUM_IMAGE_BYTES).contains(&metadata.len()) {
                return Err(SshClientError::Invalid);
            }
            let mut bytes = Vec::new();
            file.take(MAXIMUM_IMAGE_BYTES + 1)
                .read_to_end(&mut bytes)
                .map_err(|_| SshClientError::Io)?;
            if bytes.len() as u64 > MAXIMUM_IMAGE_BYTES
                || Sha256::digest(&bytes).as_slice() != digest
                || !same_cpu_elf(&bytes)
            {
                return Err(SshClientError::Invalid);
            }
            Ok(Self {
                bytes: bytes.into(),
                digest,
            })
        })
        .await
        .map_err(|_| SshClientError::Io)?
    }
    /// Returns the immutable deployment digest.
    pub fn digest(&self) -> [u8; 32] {
        self.digest
    }
}
fn same_cpu_elf(bytes: &[u8]) -> bool {
    let machine = match std::env::consts::ARCH {
        "x86_64" => 62,
        "aarch64" => 183,
        _ => return false,
    };
    if !cfg!(target_os = "linux")
        || bytes.len() < 64
        || &bytes[..7] != b"\x7fELF\x02\x01\x01"
        || !matches!(bytes[7], 0 | 3)
        || u16::from_le_bytes([bytes[18], bytes[19]]) != machine
        || !matches!(u16::from_le_bytes([bytes[16], bytes[17]]), 2 | 3)
        || u16::from_le_bytes([bytes[52], bytes[53]]) != 64
    {
        return false;
    }
    let offset = u64::from_le_bytes(bytes[32..40].try_into().expect("bounded header"));
    let Ok(offset) = usize::try_from(offset) else {
        return false;
    };
    let size = usize::from(u16::from_le_bytes([bytes[54], bytes[55]]));
    let count = usize::from(u16::from_le_bytes([bytes[56], bytes[57]]));
    if size != 56 || count == 0 || count > 1024 {
        return false;
    }
    let Some(end) = count
        .checked_mul(size)
        .and_then(|length| offset.checked_add(length))
    else {
        return false;
    };
    let Some(headers) = bytes.get(offset..end) else {
        return false;
    };
    let mut loadable = false;
    for header in headers.chunks_exact(size) {
        match u32::from_le_bytes(header[..4].try_into().expect("bounded program header")) {
            3 => return false, // PT_INTERP requires a target loader absent from the static contract.
            1 => loadable = true,
            _ => {}
        }
    }
    loadable
}

/// Published SSH execution owner; cloning its client retains the actual connection.
#[derive(Debug)]
pub struct ConnectedSsh {
    client: ProcessConnection,
    lifetime: Arc<Lifetime>,
    unavailable: Vec<String>,
}
impl ConnectedSsh {
    /// Returns a client retaining this exact SSH child and epoch.
    pub fn client(&self) -> ProcessConnection {
        self.client.clone()
    }
    /// Returns explicitly requested programs missing on the target.
    pub fn unavailable(&self) -> &[String] {
        &self.unavailable
    }
    /// Closes this epoch and waits for local SSH reaping, not proof of remote cleanup.
    pub async fn shutdown(&self) -> Result<()> {
        self.lifetime.close();
        self.lifetime.join().await
    }
}
#[derive(Debug)]
pub(crate) struct Lifetime {
    connection: Connection,
    stop: CancellationToken,
    reaped: tokio::sync::watch::Receiver<Reaped>,
}
impl Lifetime {
    fn close(&self) {
        self.connection.close();
        self.stop.cancel();
    }
    async fn join(&self) -> Result<()> {
        let mut reaped = self.reaped.clone();
        loop {
            if let Some(result) = *reaped.borrow_and_update() {
                return result.map(|_| ()).map_err(|()| SshClientError::Io);
            }
            reaped.changed().await.map_err(|_| SshClientError::Io)?;
        }
    }
}
impl Drop for Lifetime {
    fn drop(&mut self) {
        self.close();
    }
}
struct CancelOnDrop(Option<CancellationToken>);
impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        if let Some(stop) = &self.0 {
            stop.cancel();
        }
    }
}

/// Connects an explicitly admitted target using a fixed, digest-verified bootstrap.
/// The caller supplies a fresh nonzero epoch, its persistent Service namespace,
/// a distribution-verified helper and an explicitly selected program policy.
pub async fn connect(
    prepared: PreparedSsh,
    ssh: &Path,
    artifact: HelperArtifact,
    service: &str,
    epoch: u64,
    configuration: Initialization,
) -> Result<ConnectedSsh> {
    configuration
        .validate()
        .map_err(|_| SshClientError::Invalid)?;
    let bootstrap = bootstrap(service, epoch, artifact.digest, artifact.bytes.len())?;
    let mut command = prepared.command(ssh, &bootstrap)?;
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let (send, receive) = tokio::sync::oneshot::channel();
    tokio::spawn(async move {
        let result = tokio::time::timeout(
            CONNECT_DEADLINE,
            establish(prepared, command, artifact, epoch, configuration),
        )
        .await
        .unwrap_or(Err(SshClientError::Initialization));
        // Dropping an unpublished result drops its connection authority and reaps.
        let _ = send.send(result);
    });
    receive.await.map_err(|_| SshClientError::Io)?
}
async fn establish(
    prepared: PreparedSsh,
    command: std::process::Command,
    artifact: HelperArtifact,
    epoch: u64,
    configuration: Initialization,
) -> Result<ConnectedSsh> {
    let mut child = tokio::process::Command::from(command)
        .kill_on_drop(true)
        .spawn()
        .map_err(|_| SshClientError::Io)?;
    let mut input = child.stdin.take().ok_or(SshClientError::Io)?;
    let output = child.stdout.take().ok_or(SshClientError::Io)?;
    let errors = child.stderr.take().ok_or(SshClientError::Io)?;
    let stop = CancellationToken::new();
    let mut guard = CancelOnDrop(Some(stop.clone()));
    let (send, reaped) = tokio::sync::watch::channel(None);
    tokio::spawn(supervise(prepared, child, errors, stop.clone(), send));
    let uploaded = tokio::select! {
        () = stop.cancelled() => Err(SshClientError::Initialization),
        result = input.write_all(&artifact.bytes) => result.map_err(|_| SshClientError::Io),
    };
    if let Err(error) = uploaded {
        drop(input);
        drop(output);
        return Err(startup_failure(&stop, &reaped, error).await);
    }
    let Ok((connection, _)) = Connection::start(output, input, Role::Client, epoch) else {
        return Err(startup_failure(&stop, &reaped, SshClientError::Initialization).await);
    };
    let lifetime = Arc::new(Lifetime {
        connection: connection.clone(),
        stop: stop.clone(),
        reaped,
    });
    tokio::spawn(heartbeat(connection.clone(), stop));
    let client = ProcessConnection::managed(connection, lifetime.clone());
    let Ok(unavailable) = client.initialize(configuration, &artifact.digest).await else {
        lifetime.close();
        return Err(startup_failure(
            &lifetime.stop,
            &lifetime.reaped,
            SshClientError::Initialization,
        )
        .await);
    };
    // Ownership transfers to Lifetime only after the handshake succeeds.
    guard.0.take();
    Ok(ConnectedSsh {
        client,
        lifetime,
        unavailable,
    })
}
async fn supervise(
    _prepared: PreparedSsh,
    mut child: tokio::process::Child,
    errors: tokio::process::ChildStderr,
    stop: CancellationToken,
    reaped: tokio::sync::watch::Sender<Reaped>,
) {
    let mut drain = tokio::spawn(drain_errors(errors, stop.clone()));
    let result = tokio::select! {
        result = child.wait() => result,
        () = stop.cancelled() => {
            if let Ok(result) = tokio::time::timeout(Duration::from_secs(3), child.wait()).await {
                result
            } else { let _ = child.start_kill(); child.wait().await }
        }
    };
    stop.cancel();
    if tokio::time::timeout(Duration::from_secs(1), &mut drain)
        .await
        .is_err()
    {
        drain.abort();
        let _ = drain.await;
    }
    reaped.send_replace(Some(result.map_err(|_| ())));
}
async fn startup_failure(
    stop: &CancellationToken,
    reaped: &tokio::sync::watch::Receiver<Reaped>,
    fallback: SshClientError,
) -> SshClientError {
    stop.cancel();
    let mut reaped = reaped.clone();
    let deadline = tokio::time::Instant::now() + STARTUP_REAP_DEADLINE;
    loop {
        if let Some(result) = *reaped.borrow_and_update() {
            return if result.is_ok_and(|status| {
                status.code() == Some(i32::from(rsi_ssh_protocol::CACHE_CONTENTION_EXIT_CODE))
            }) {
                SshClientError::CacheContentionTimeout
            } else {
                fallback
            };
        }
        if !matches!(
            tokio::time::timeout_at(deadline, reaped.changed()).await,
            Ok(Ok(()))
        ) {
            return fallback;
        }
    }
}
async fn drain_errors(mut errors: tokio::process::ChildStderr, stop: CancellationToken) {
    let mut buffer = [0u8; 4096];
    let mut total = 0usize;
    loop {
        match errors.read(&mut buffer).await {
            Ok(0) | Err(_) => return,
            Ok(count) => {
                total = total.saturating_add(count);
                if total > 64 * 1024 {
                    stop.cancel();
                    return;
                }
            }
        }
    }
}
async fn heartbeat(connection: Connection, stop: CancellationToken) {
    loop {
        tokio::select! {
            () = stop.cancelled() => break,
            () = connection.closed() => break,
            () = tokio::time::sleep(Duration::from_secs(2)) => {
                if connection.heartbeat().await.is_err() { break; }
            }
        }
    }
    connection.close();
    stop.cancel();
    connection.settled().await;
}
fn bootstrap(service: &str, epoch: u64, digest: [u8; 32], length: usize) -> Result<String> {
    if service.len() != 32
        || !service
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        || epoch == 0
        || !(64..=MAXIMUM_IMAGE_BYTES).contains(&(length as u64))
    {
        return Err(SshClientError::Invalid);
    }
    let cpu = match std::env::consts::ARCH {
        "x86_64" => "x86_64",
        "aarch64" => "aarch64",
        _ => return Err(SshClientError::Unsupported),
    };
    let script = format!(
        r#"set -eu
umask 077
test "$(/usr/bin/uname -s)" = Linux
test "$(/usr/bin/uname -m)" = {cpu}
uid=$(/usr/bin/id -u)
runtime=/run/user/$uid
test -d "$runtime"
test ! -L "$runtime"
test "$(/usr/bin/stat -c '%u:%a' -- "$runtime")" = "$uid:700"
stage=$(/usr/bin/mktemp -d "$runtime/rsi-upload.XXXXXXXXXX")
trap '/usr/bin/rm -rf -- "$stage"' EXIT
trap 'exit 1' HUP INT TERM
/usr/bin/head -c {length} > "$stage/helper"
test "$(/usr/bin/stat -c '%s' -- "$stage/helper")" = {length}
printf '%s  %s\n' '{digest}' "$stage/helper" | /usr/bin/sha256sum --status -c -
/usr/bin/chmod 500 "$stage/helper"
"$stage/helper" install-launch {service} {epoch} {digest}
"#,
        digest = hex::encode(digest)
    );
    Ok(format!(
        "exec /usr/bin/env -i /bin/sh -c '{}'",
        script.replace('\'', "'\\''")
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test(start_paused = true)]
    async fn missing_reap_evidence_returns_the_original_failure_within_local_deadline() {
        let (_sender, reaped) = tokio::sync::watch::channel(None);
        let stop = CancellationToken::new();
        let result = tokio::time::timeout(
            Duration::from_secs(6),
            startup_failure(&stop, &reaped, SshClientError::Initialization),
        )
        .await
        .expect("startup classification must not wait indefinitely for a reap receipt");
        assert!(matches!(result, SshClientError::Initialization));
        assert!(stop.is_cancelled());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn only_reaped_exit_75_is_cache_contention_and_other_failures_are_not_replayed() {
        for (script, contention) in [
            ("exit 75", true),
            ("exit 255", false),
            ("kill -TERM $$", false),
        ] {
            let status = tokio::process::Command::new("/bin/sh")
                .args(["-c", script])
                .status()
                .await
                .unwrap();
            let (_sender, reaped) = tokio::sync::watch::channel(Some(Ok(status)));
            let stop = CancellationToken::new();
            let result = startup_failure(&stop, &reaped, SshClientError::Initialization).await;
            assert!(stop.is_cancelled());
            assert_eq!(
                matches!(result, SshClientError::CacheContentionTimeout),
                contention
            );
        }
        let (_sender, reaped) = tokio::sync::watch::channel(Some(Err(())));
        assert!(matches!(
            startup_failure(&CancellationToken::new(), &reaped, SshClientError::Io).await,
            SshClientError::Io
        ));
    }
    #[test]
    fn bootstrap_rejects_shell_syntax_and_places_verification_before_execution() {
        assert!(bootstrap("$(touch /tmp/unsafe)", 1, [0; 32], 64).is_err());
        assert!(bootstrap(&"a".repeat(32), 0, [0; 32], 64).is_err());
        assert!(bootstrap(&"a".repeat(32), 1, [0; 32], usize::MAX).is_err());
        let command = bootstrap(&"a".repeat(32), 1, [0; 32], 64).unwrap();
        assert!(command.find("sha256sum").unwrap() < command.find("chmod").unwrap());
        assert!(command.find("chmod").unwrap() < command.find("install-launch").unwrap());
    }
    #[test]
    fn static_helper_rejects_target_loader_and_truncated_program_headers() {
        let mut bytes = vec![0; 64 + 56];
        bytes[..7].copy_from_slice(b"\x7fELF\x02\x01\x01");
        bytes[16..18].copy_from_slice(&3u16.to_le_bytes());
        let machine: u16 = match std::env::consts::ARCH {
            "x86_64" => 62,
            "aarch64" => 183,
            _ => return,
        };
        bytes[18..20].copy_from_slice(&machine.to_le_bytes());
        bytes[32..40].copy_from_slice(&64u64.to_le_bytes());
        bytes[52..54].copy_from_slice(&64u16.to_le_bytes());
        bytes[54..56].copy_from_slice(&56u16.to_le_bytes());
        bytes[56..58].copy_from_slice(&1u16.to_le_bytes());
        bytes[64..68].copy_from_slice(&1u32.to_le_bytes());
        assert_eq!(same_cpu_elf(&bytes), cfg!(target_os = "linux"));
        for abi in [0, 3, 6, 9, 255] {
            bytes[7] = abi;
            assert_eq!(
                same_cpu_elf(&bytes),
                cfg!(target_os = "linux") && matches!(abi, 0 | 3)
            );
        }
        bytes[7] = 0;
        bytes[64..68].copy_from_slice(&3u32.to_le_bytes());
        assert!(!same_cpu_elf(&bytes));
        bytes[64..68].copy_from_slice(&1u32.to_le_bytes());
        bytes.pop();
        assert!(!same_cpu_elf(&bytes));
        bytes[32..40].copy_from_slice(&u64::MAX.to_le_bytes());
        assert!(!same_cpu_elf(&bytes));
    }
    #[tokio::test]
    async fn artifact_rejects_wrong_digest_and_non_native_image() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("image");
        let bytes = [0; 64];
        std::fs::write(&path, bytes).unwrap();
        assert!(
            HelperArtifact::load(&path, Sha256::digest(bytes).into())
                .await
                .is_err()
        );
        assert!(HelperArtifact::load(&path, [1; 32]).await.is_err());
    }
}
