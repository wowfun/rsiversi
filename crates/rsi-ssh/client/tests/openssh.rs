#![cfg(target_os = "linux")]
use rsi_ssh_client::PreparedSsh;
use rsi_ssh_protocol::{SshEndpoint, SshHostKey};
use std::{
    fs,
    net::{TcpListener, TcpStream},
    os::unix::{fs::PermissionsExt, net::UnixListener},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    time::Duration,
};

struct Server(Child);
impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
fn env_path(name: &str) -> PathBuf {
    PathBuf::from(
        std::env::var_os(name).unwrap_or_else(|| panic!("explicit opt-in requires {name}")),
    )
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
    fs::read_to_string(path.with_extension("pub"))
        .unwrap()
        .trim_end()
        .to_owned()
}
fn namespace(home: &Path, fixture: &Path) -> Command {
    let real_home = env_path("HOME");
    let mut command = Command::new("/usr/bin/bwrap");
    command
        .env_clear()
        .args([
            "--die-with-parent",
            "--unshare-pid",
            "--ro-bind",
            "/",
            "/",
            "--dev",
            "/dev",
            "--proc",
            "/proc",
        ])
        .arg("--bind")
        .arg(fixture)
        .arg(fixture)
        .arg("--bind")
        .arg(home)
        .arg(real_home)
        .arg("--ro-bind")
        .arg(fixture.join("system-config"))
        .arg("/etc/ssh/ssh_config")
        .arg("--chdir")
        .arg(fixture);
    command
}

#[test]
#[ignore = "requires explicit RSI_TEST_SSHD/SSH_USER/SSH_FIXTURES and native Linux Bubblewrap"]
#[allow(clippy::too_many_lines)] // One isolated fixture proves actual config exclusion and pinned-key authentication.
fn generated_configuration_ignores_hostile_home_and_agent_and_rejects_a_changed_host_key() {
    let temporary = tempfile::Builder::new()
        .permissions(fs::Permissions::from_mode(0o700))
        .tempdir()
        .unwrap();
    let root = temporary.path();
    let home = root.join("home");
    fs::create_dir_all(home.join(".ssh")).unwrap();
    let host = root.join("host");
    let original_identity = root.join("identity");
    let other = root.join("other");
    key(&host);
    key(&original_identity);
    key(&other);
    // StrictModes checks all authorized_keys ancestors; /tmp is intentionally
    // world-writable, so place this fixture under an explicit private home subtree.
    let authorized_root = tempfile::Builder::new()
        .permissions(fs::Permissions::from_mode(0o700))
        .tempdir_in(env_path("RSI_TEST_SSH_FIXTURES"))
        .unwrap();
    let authorized = authorized_root.path().join("authorized_keys");
    fs::write(&authorized, public(&original_identity)).unwrap();
    fs::set_permissions(&authorized, fs::Permissions::from_mode(0o600)).unwrap();
    let identity = root.join("identity with \"quote\" and \\slash");
    fs::rename(&original_identity, &identity).unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    let user = std::env::var("RSI_TEST_SSH_USER").expect("explicit RSI_TEST_SSH_USER");
    let config = root.join("sshd_config");
    fs::write(&config, format!("Port {port}\nListenAddress 127.0.0.1\nHostKey {}\nPidFile {}\nAuthorizedKeysFile {}\n{}PasswordAuthentication no\nKbdInteractiveAuthentication no\nAuthenticationMethods publickey\nUsePAM no\nStrictModes yes\nAllowUsers {user}\nAllowTcpForwarding no\nAllowAgentForwarding no\nX11Forwarding no\nPermitTunnel no\nPermitUserEnvironment no\n", host.display(), root.join("sshd.pid").display(), authorized.display(), sshd_subprocess_paths())).unwrap();
    let mut daemon = Command::new(env_path("RSI_TEST_SSHD"));
    daemon
        .env_clear()
        .args(["-D", "-e", "-f"])
        .arg(config)
        .stdout(Stdio::null())
        .stderr(fs::File::create(root.join("sshd.log")).unwrap());
    if let Some(path) = std::env::var_os("RSI_TEST_SSH_LIBRARY_PATH") {
        daemon.env("LD_LIBRARY_PATH", path);
    }
    let mut server = Server(daemon.spawn().unwrap());
    let mut listening = false;
    for _ in 0..200 {
        assert!(
            server.0.try_wait().unwrap().is_none(),
            "isolated sshd exited"
        );
        if TcpStream::connect(("127.0.0.1", port)).is_ok() {
            listening = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(listening, "isolated sshd startup deadline");
    let marker = root.join("ambient-command-ran");
    let system_marker = root.join("system-command-ran");
    fs::write(
        root.join("system-config"),
        format!(
            "Match exec \"/usr/bin/touch {}\"\n",
            system_marker.display()
        ),
    )
    .unwrap();
    fs::write(home.join(".ssh/config"), format!("Match exec \"/usr/bin/touch {}\"\nHost *\n    User unintended-account\n    IdentityFile /missing/identity\n    ProxyCommand /usr/bin/false\n    PermitLocalCommand yes\n    LocalCommand /usr/bin/touch {}\n", marker.display(), marker.display())).unwrap();
    // A positive control proves that this namespace really substitutes the account's
    // home configuration, rather than assuming OpenSSH uses the HOME variable.
    let canary = namespace(&home, root)
        .args(["/usr/bin/ssh", "-G", "rsi-target"])
        .output()
        .unwrap();
    assert!(
        canary.status.success(),
        "{}",
        String::from_utf8_lossy(&canary.stderr)
    );
    assert!(
        marker.exists(),
        "hostile config positive control did not run"
    );
    fs::remove_file(&marker).unwrap();
    assert!(
        system_marker.exists(),
        "hostile system configuration positive control did not run"
    );
    fs::remove_file(&system_marker).unwrap();
    let agent_path = root.join("agent.sock");
    let agent = UnixListener::bind(&agent_path).unwrap();
    agent.set_nonblocking(true).unwrap();
    let endpoint = SshEndpoint::new("127.0.0.1", port, user).unwrap();
    let host_key = SshHostKey::parse(&public(&host)).unwrap();
    let fingerprint = Command::new("/usr/bin/ssh-keygen")
        .args(["-l", "-E", "sha256", "-f"])
        .arg(host.with_extension("pub"))
        .output()
        .unwrap();
    assert!(fingerprint.status.success());
    assert!(String::from_utf8_lossy(&fingerprint.stdout).contains(&host_key.fingerprint()));
    let prepared = PreparedSsh::new(&endpoint, &host_key, &identity).unwrap();
    assert_eq!(
        fs::metadata(prepared.directory())
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o700
    );
    assert_eq!(
        fs::metadata(prepared.configuration_path())
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    let exact = prepared
        .command(Path::new("/usr/bin/ssh"), "printf 'SSH_CONFIG_VERIFIED\\n'")
        .unwrap();
    assert_eq!(exact.get_envs().count(), 0);
    // The configuration lives in a separately private temporary directory, so bind
    // it writable only for OpenSSH's normal access checks inside the same namespace.
    let mut command = namespace(&home, root);
    command
        .arg("--ro-bind")
        .arg(prepared.directory())
        .arg(prepared.directory())
        .arg(exact.get_program())
        .args(exact.get_args())
        .env("SSH_AUTH_SOCK", &agent_path)
        .env("SSH_ASKPASS", "/usr/bin/false");
    let result = command.output().unwrap();
    assert!(
        result.status.success(),
        "{}\nServer: {}",
        String::from_utf8_lossy(&result.stderr),
        fs::read_to_string(root.join("sshd.log")).unwrap()
    );
    assert_eq!(result.stdout, b"SSH_CONFIG_VERIFIED\n");
    assert!(!marker.exists());
    assert!(!system_marker.exists());
    assert_eq!(
        agent.accept().unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock
    );
    let wrong = PreparedSsh::new(
        &endpoint,
        &SshHostKey::parse(&public(&other)).unwrap(),
        &identity,
    )
    .unwrap();
    let forbidden = wrong
        .command(Path::new("/usr/bin/ssh"), "printf SHOULD_NOT_EXECUTE")
        .unwrap();
    let result = namespace(&home, root)
        .arg("--ro-bind")
        .arg(wrong.directory())
        .arg(wrong.directory())
        .arg(forbidden.get_program())
        .args(forbidden.get_args())
        .output()
        .unwrap();
    assert!(!result.status.success());
    assert!(result.stdout.is_empty());
    assert!(!marker.exists());
    assert!(!system_marker.exists());
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
