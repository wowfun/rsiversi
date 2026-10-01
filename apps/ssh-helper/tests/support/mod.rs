use rsi_ssh_client::PreparedSsh;
use rsi_ssh_protocol::{SshEndpoint, SshHostKey};
use std::{
    fs,
    net::{TcpListener, TcpStream},
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    time::Duration,
};

pub struct Server {
    child: Child,
    _directory: tempfile::TempDir,
    _authorized: tempfile::TempDir,
    endpoint: SshEndpoint,
    key: SshHostKey,
    identity: PathBuf,
}
impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
fn env_path(name: &str) -> PathBuf {
    std::env::var_os(name)
        .unwrap_or_else(|| panic!("explicit opt-in requires {name}"))
        .into()
}
fn key(path: &Path) {
    assert!(
        Command::new("/usr/bin/ssh-keygen")
            .args(["-q", "-t", "ed25519", "-N", "", "-f"])
            .arg(path)
            .status()
            .unwrap()
            .success()
    );
}
fn public(path: &Path) -> String {
    fs::read_to_string(path.with_extension("pub")).unwrap()
}
impl Server {
    pub fn new() -> Self {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path();
        let host = root.join("host");
        let identity = root.join("identity");
        key(&host);
        key(&identity);
        let authorized = tempfile::Builder::new()
            .permissions(fs::Permissions::from_mode(0o700))
            .tempdir_in(env_path("RSI_TEST_SSH_FIXTURES"))
            .unwrap();
        let authorized_keys = authorized.path().join("authorized_keys");
        fs::write(&authorized_keys, public(&identity)).unwrap();
        fs::set_permissions(&authorized_keys, fs::Permissions::from_mode(0o600)).unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        let user = std::env::var("RSI_TEST_SSH_USER").expect("explicit RSI_TEST_SSH_USER");
        let config = root.join("sshd_config");
        fs::write(&config, format!("Port {port}\nListenAddress 127.0.0.1\nHostKey {}\nPidFile {}\nAuthorizedKeysFile {}\n{}PasswordAuthentication no\nKbdInteractiveAuthentication no\nAuthenticationMethods publickey\nUsePAM no\nStrictModes yes\nAllowUsers {user}\nAllowTcpForwarding no\nAllowAgentForwarding no\nX11Forwarding no\nPermitTunnel no\nPermitUserEnvironment no\n", host.display(), root.join("sshd.pid").display(), authorized_keys.display(), sshd_subprocess_paths())).unwrap();
        let mut command = Command::new(env_path("RSI_TEST_SSHD"));
        command
            .env_clear()
            .args(["-D", "-e", "-f"])
            .arg(config)
            .stdout(Stdio::null())
            .stderr(fs::File::create(root.join("sshd.log")).unwrap());
        if let Some(path) = std::env::var_os("RSI_TEST_SSH_LIBRARY_PATH") {
            command.env("LD_LIBRARY_PATH", path);
        }
        let mut server = Self {
            child: command.spawn().unwrap(),
            endpoint: SshEndpoint::new("127.0.0.1", port, user).unwrap(),
            key: SshHostKey::parse(public(&host).trim()).unwrap(),
            identity,
            _directory: directory,
            _authorized: authorized,
        };
        for _ in 0..200 {
            assert!(
                server.child.try_wait().unwrap().is_none(),
                "isolated sshd exited"
            );
            if TcpStream::connect(("127.0.0.1", port)).is_ok() {
                return server;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        panic!("isolated sshd startup deadline");
    }
    pub fn prepare(&self) -> PreparedSsh {
        PreparedSsh::new(&self.endpoint, &self.key, &self.identity).unwrap()
    }
}

fn sshd_subprocess_paths() -> String {
    [
        ("RSI_TEST_SSH_SESSION", "SshdSessionPath"),
        ("RSI_TEST_SSH_AUTH", "SshdAuthPath"),
    ]
    .into_iter()
    .filter_map(|(variable, directive)| {
        std::env::var_os(variable).map(|path| {
            let path = std::path::PathBuf::from(path);
            let text = path.to_str().expect("UTF-8 fixture path");
            assert!(path.is_absolute() && !text.chars().any(char::is_whitespace));
            format!("{directive} {text}\n")
        })
    })
    .collect()
}
