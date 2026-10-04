//! Private generated OpenSSH configuration for an explicitly trusted target.
#![forbid(unsafe_code)]
#![warn(missing_docs)]
#![allow(clippy::missing_errors_doc)]
mod connection;
mod process;
pub use connection::{ConnectedSsh, HelperArtifact, connect};
pub use process::{
    ProcessConnection, RemoteFiles, RemotePlan, RemotePtyProcess, execution_provider,
};
use rsi_ssh_protocol::{SshEndpoint, SshHostKey};
use std::{
    io::Write,
    path::{Path, PathBuf},
    process::Command,
};

/// Configuration preparation failure without exposing identity contents.
#[derive(Debug, thiserror::Error)]
pub enum SshClientError {
    /// This implementation requires a native Linux Service.
    #[error("SSH execution requires a Linux Service")]
    Unsupported,
    /// A path or remote command cannot be represented literally.
    #[error("invalid explicit SSH invocation input")]
    Invalid,
    /// Private configuration could not be created.
    #[error("SSH private configuration I/O failed")]
    Io,
    /// Bootstrap or target prerequisites failed without publishing execution authority.
    #[error("SSH helper initialization failed")]
    Initialization,
    /// Remote cache writer admission expired; bootstrap must not be replayed automatically.
    #[error("SSH helper cache contention deadline exceeded")]
    CacheContentionTimeout,
}
/// Prepared client result.
pub type Result<T> = std::result::Result<T, SshClientError>;

/// Owned configuration retained through the SSH child lifetime.
/// It may retain a private identity copy whose lifetime is exactly the connection.
pub struct PreparedSsh {
    directory: tempfile::TempDir,
    configuration: PathBuf,
}
impl std::fmt::Debug for PreparedSsh {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PreparedSsh").finish_non_exhaustive()
    }
}
impl PreparedSsh {
    /// Copies an already verified bounded Local identity into connection-private storage.
    /// The product trust owner must verify the exact bytes before calling this method.
    pub fn with_identity(
        endpoint: &SshEndpoint,
        host_key: &SshHostKey,
        identity: &[u8],
    ) -> Result<Self> {
        if identity.is_empty() || identity.len() > 64 * 1024 {
            return Err(SshClientError::Invalid);
        }
        let directory = private_directory()?;
        let path = directory.path().join("identity");
        write_private(&path, identity)?;
        Self::in_directory(directory, endpoint, host_key, &path)
    }
    /// Creates private generated inputs from a trusted endpoint, pinned public key and identity.
    /// Authorization and trust confirmation must precede this call in the product owner.
    pub fn new(endpoint: &SshEndpoint, host_key: &SshHostKey, identity: &Path) -> Result<Self> {
        Self::in_directory(private_directory()?, endpoint, host_key, identity)
    }
    fn in_directory(
        directory: tempfile::TempDir,
        endpoint: &SshEndpoint,
        host_key: &SshHostKey,
        identity: &Path,
    ) -> Result<Self> {
        let identity = quoted_path(identity)?;
        let configuration = directory.path().join("config");
        let known_hosts = directory.path().join("known_hosts");
        let known = format!(
            "rsi-target {} {}\n",
            host_key.algorithm(),
            host_key.base64()
        );
        write_private(&known_hosts, known.as_bytes())?;
        let config = format!(
            "Host rsi-target\n    HostName {}\n    Port {}\n    User {}\n    HostKeyAlias rsi-target\n    HostKeyAlgorithms {}\n    UserKnownHostsFile {}\n    GlobalKnownHostsFile /dev/null\n    IdentityFile {}\n{}",
            endpoint.host(),
            endpoint.port(),
            endpoint.user(),
            host_key.signature_algorithms(),
            quoted_path(&known_hosts)?,
            identity,
            FIXED_POLICY
        );
        write_private(&configuration, config.as_bytes())?;
        Ok(Self {
            directory,
            configuration,
        })
    }
    /// Builds an exact SSH child with no inherited environment.
    /// The transport owner supplies its fixed bootstrap/helper command, never target configuration.
    pub fn command(&self, executable: &Path, remote_command: &str) -> Result<Command> {
        if !executable.is_absolute()
            || remote_command.is_empty()
            || remote_command.len() > 64 * 1024
            || remote_command.contains('\0')
        {
            return Err(SshClientError::Invalid);
        }
        let mut command = Command::new(executable);
        command
            .env_clear()
            .arg("-F")
            .arg(&self.configuration)
            .arg("-T")
            .arg("--")
            .arg("rsi-target")
            .arg(remote_command);
        Ok(command)
    }
    /// Returns the generated file for configuration diagnostics without opening ambient config.
    pub fn configuration_path(&self) -> &Path {
        &self.configuration
    }
    /// Returns the private directory whose lifetime the SSH owner must retain.
    pub fn directory(&self) -> &Path {
        self.directory.path()
    }
}

fn private_directory() -> Result<tempfile::TempDir> {
    if !cfg!(target_os = "linux") {
        return Err(SshClientError::Unsupported);
    }
    let mut builder = tempfile::Builder::new();
    builder.prefix("rsi-ssh-");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        builder.permissions(std::fs::Permissions::from_mode(0o700));
    }
    builder.tempdir().map_err(|_| SshClientError::Io)
}

const FIXED_POLICY: &str = "    BatchMode yes\n    StrictHostKeyChecking yes\n    CheckHostIP no\n    UpdateHostKeys no\n    VerifyHostKeyDNS no\n    IdentityAgent none\n    CertificateFile none\n    PKCS11Provider none\n    IdentitiesOnly yes\n    ForwardAgent no\n    ForwardX11 no\n    ClearAllForwardings yes\n    ExitOnForwardFailure yes\n    ControlMaster no\n    ControlPath none\n    ControlPersist no\n    ProxyCommand none\n    ProxyJump none\n    PermitLocalCommand no\n    LocalCommand none\n    CanonicalizeHostname no\n    RequestTTY no\n    PasswordAuthentication no\n    KbdInteractiveAuthentication no\n    GSSAPIAuthentication no\n    HostbasedAuthentication no\n    PreferredAuthentications publickey\n    PubkeyAuthentication yes\n    NumberOfPasswordPrompts 0\n    ConnectionAttempts 1\n    ConnectTimeout 10\n    ServerAliveInterval 5\n    ServerAliveCountMax 2\n    LogLevel ERROR\n";
fn quoted_path(path: &Path) -> Result<String> {
    let value = path.to_str().ok_or(SshClientError::Invalid)?;
    if !path.is_absolute()
        || value.len() > 16 * 1024
        || value.chars().any(char::is_control)
        || value.contains(['%', '$'])
    {
        return Err(SshClientError::Invalid);
    }
    Ok(format!(
        "\"{}\"",
        value.replace('\\', "\\\\").replace('"', "\\\"")
    ))
}
fn write_private(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path).map_err(|_| SshClientError::Io)?;
    file.write_all(bytes).map_err(|_| SshClientError::Io)
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    #[test]
    fn copied_identity_is_private_immutable_and_owned_by_preparation() {
        use std::os::unix::fs::PermissionsExt;
        let key = SshHostKey::parse(
            "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIAcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcH",
        )
        .unwrap();
        let endpoint = SshEndpoint::new("127.0.0.1", 22, "fixture").unwrap();
        let mut bytes = b"fixture-private-key".to_vec();
        let prepared = PreparedSsh::with_identity(&endpoint, &key, &bytes).unwrap();
        let path = prepared.directory().join("identity");
        bytes.fill(0);
        assert_eq!(std::fs::read(&path).unwrap(), b"fixture-private-key");
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        let config = std::fs::read_to_string(prepared.configuration_path()).unwrap();
        assert!(config.contains(&format!("IdentityFile {}", quoted_path(&path).unwrap())));
        assert!(!format!("{prepared:?}").contains("fixture-private-key"));
        drop(prepared);
        assert!(!path.exists());
        assert!(PreparedSsh::with_identity(&endpoint, &key, &vec![0; 64 * 1024 + 1]).is_err());
    }
    #[test]
    fn path_quoting_is_literal_and_rejects_ssh_expansions() {
        assert_eq!(
            quoted_path(Path::new("/key directory/a\"b\\c")).unwrap(),
            "\"/key directory/a\\\"b\\\\c\""
        );
        for path in [
            "relative",
            "/key%h",
            "/key${HOME}",
            "/key\nInclude /evil",
            "/key\0",
        ] {
            assert!(quoted_path(Path::new(path)).is_err());
        }
    }
}
