use crate::{HelperError, Result};
use rsi_ssh_transport::{Connection, Role};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::Duration,
};
use tokio::{io::AsyncReadExt, net::UnixDatagram};

const PROPERTIES: &[(&str, &str, &str)] = &[
    ("Type", "notify", "notify"),
    ("NotifyAccess", "main", "main"),
    ("Restart", "no", "no"),
    ("ExitType", "main", "main"),
    ("RemainAfterExit", "no", "no"),
    ("WatchdogSec", "30s", "30s"),
    ("WatchdogSignal", "SIGTERM", "15"),
    ("TimeoutStartSec", "10s", "10s"),
    ("TimeoutStopSec", "2s", "2s"),
    ("TimeoutAbortSec", "2s", "2s"),
    ("KillMode", "control-group", "control-group"),
    ("SendSIGKILL", "yes", "yes"),
];
const INSPECTION_BYTES: usize = 16 * 1024;

fn queried_name(name: &str) -> String {
    if let Some(stem) = name.strip_suffix("Sec") {
        format!("{stem}USec")
    } else {
        name.into()
    }
}
pub(crate) fn runtime_directory() -> PathBuf {
    PathBuf::from(format!("/run/user/{}", rustix::process::getuid().as_raw()))
}

/// Fixed isolated transient unit identity. It provides no target authorization.
#[derive(Clone, Debug)]
pub struct TransientUnit {
    name: String,
    epoch: u64,
}
impl TransientUnit {
    /// Binds a validated Service namespace and epoch to one fresh unit name.
    pub fn new(service: &str, epoch: u64) -> Result<Self> {
        if !super::valid_service_namespace(service) || epoch == 0 {
            return Err(HelperError::Invalid);
        }
        Ok(Self {
            name: format!("rsi-ssh-{service}-{epoch:016x}.service"),
            epoch,
        })
    }
    /// Returns the exact manager-owned unit name.
    pub fn name(&self) -> &str {
        &self.name
    }
    /// Returns the matching connection epoch.
    pub fn epoch(&self) -> u64 {
        self.epoch
    }
    /// Builds the fixed launcher around an explicit target executable. The trusted
    /// app may append only its own helper arguments, never raw target configuration.
    pub fn command(&self, executable: &Path) -> Result<Command> {
        if !executable.is_absolute()
            || executable.as_os_str().len() > 16 * 1024
            || executable.as_os_str().as_encoded_bytes().contains(&0)
        {
            return Err(HelperError::Invalid);
        }
        let mut command = Command::new("/usr/bin/systemd-run");
        command
            .env_clear()
            .env("XDG_RUNTIME_DIR", runtime_directory())
            .args([
                "--user",
                "--pipe",
                "--wait",
                "--collect",
                "--expand-environment=no",
            ])
            .arg(format!("--unit={}", self.name));
        for (name, value, _) in PROPERTIES {
            command.arg(format!("--property={name}={value}"));
        }
        command.arg("--").arg(executable);
        Ok(command)
    }
}

/// Verified main-process notifier. This is separate from normal child environment policy.
#[derive(Debug)]
pub struct SystemdWatchdog {
    socket: UnixDatagram,
    epoch: u64,
}
impl SystemdWatchdog {
    /// Verifies live systemd ownership and connects to its exact notification socket.
    pub async fn verify(unit: &TransientUnit) -> Result<Self> {
        let pid = std::process::id();
        if std::env::var("WATCHDOG_USEC").ok().as_deref() != Some("30000000")
            || std::env::var("WATCHDOG_PID").ok().as_deref() != Some(pid.to_string().as_str())
        {
            return Err(HelperError::Unavailable);
        }
        let expected_socket = runtime_directory().join("systemd/notify");
        if std::env::var_os("NOTIFY_SOCKET").as_deref() != Some(expected_socket.as_os_str()) {
            return Err(HelperError::Unavailable);
        }
        let cgroup_root =
            rsi_files_native_fs::open_absolute_directory_no_follow(Path::new("/sys/fs/cgroup"))
                .map_err(|_| HelperError::Unavailable)?;
        // Linux UAPI CGROUP2_SUPER_MAGIC: v1 hierarchies do not satisfy ownership.
        if rustix::fs::fstatfs(&cgroup_root)
            .map_err(|_| HelperError::Unavailable)?
            .f_type
            != 0x6367_7270
        {
            return Err(HelperError::Unavailable);
        }
        let manager = inspect(vec!["--property=ServiceWatchdogs".into()]).await?;
        if manager != "ServiceWatchdogs=yes\n" {
            return Err(HelperError::Unavailable);
        }
        let mut arguments = PROPERTIES
            .iter()
            .map(|(name, _, _)| format!("--property={}", queried_name(name)))
            .collect::<Vec<_>>();
        arguments.extend([
            "--property=MainPID".into(),
            "--property=ControlGroup".into(),
            "--".into(),
            unit.name.clone(),
        ]);
        let properties = inspect(arguments).await?;
        let mut cgroup = String::new();
        tokio::fs::File::open("/proc/self/cgroup")
            .await
            .map_err(|_| HelperError::Io)?
            .take(INSPECTION_BYTES as u64 + 1)
            .read_to_string(&mut cgroup)
            .await
            .map_err(|_| HelperError::Io)?;
        validate_properties(&properties, &cgroup, unit, pid)?;
        let socket = UnixDatagram::unbound().map_err(|_| HelperError::Io)?;
        socket
            .connect(&expected_socket)
            .map_err(|_| HelperError::Io)?;
        Ok(Self {
            socket,
            epoch: unit.epoch,
        })
    }
    /// Publishes readiness and feeds only from fresh evidence on the exact helper
    /// connection. Returning an error requires the helper owner to exit and settle.
    pub async fn run(self, connection: Connection) -> Result<()> {
        if connection.epoch() != self.epoch || connection.role() != Role::Helper {
            return Err(HelperError::Invalid);
        }
        let mut heartbeats = connection.heartbeats();
        self.notify(b"READY=1").await?;
        let mut last = 0;
        loop {
            let current = *heartbeats.borrow_and_update();
            if current > last {
                self.notify(b"WATCHDOG=1").await?;
                last = current;
            }
            tokio::select! {
                () = connection.closed() => return Ok(()),
                changed = heartbeats.changed() => if changed.is_err() { return Err(HelperError::Unavailable); }
            }
        }
    }
    async fn notify(&self, message: &[u8]) -> Result<()> {
        let sent = tokio::time::timeout(Duration::from_secs(1), self.socket.send(message))
            .await
            .map_err(|_| HelperError::Io)?
            .map_err(|_| HelperError::Io)?;
        if sent != message.len() {
            return Err(HelperError::Io);
        }
        Ok(())
    }
}

fn validate_properties(text: &str, cgroup: &str, unit: &TransientUnit, pid: u32) -> Result<()> {
    if text.len() > INSPECTION_BYTES || cgroup.len() > INSPECTION_BYTES {
        return Err(HelperError::Invalid);
    }
    let mut fields = BTreeMap::new();
    for line in text.lines() {
        let (key, value) = line.split_once('=').ok_or(HelperError::Invalid)?;
        if fields.insert(key, value).is_some() {
            return Err(HelperError::Invalid);
        }
    }
    if fields.len() != PROPERTIES.len() + 2 {
        return Err(HelperError::Unavailable);
    }
    for (name, _, expected) in PROPERTIES {
        if fields.get(queried_name(name).as_str()) != Some(expected) {
            return Err(HelperError::Unavailable);
        }
    }
    if fields.get("MainPID").copied() != Some(pid.to_string().as_str()) {
        return Err(HelperError::Unavailable);
    }
    let group = *fields.get("ControlGroup").ok_or(HelperError::Unavailable)?;
    if !group.starts_with("/user.slice/")
        || !group.ends_with(&format!("/{}", unit.name))
        || cgroup != format!("0::{group}\n")
    {
        return Err(HelperError::Unavailable);
    }
    Ok(())
}

async fn inspect(arguments: Vec<String>) -> Result<String> {
    let mut command = std::process::Command::new("/usr/bin/systemctl");
    command
        .env_clear()
        .env("XDG_RUNTIME_DIR", runtime_directory())
        .args(["--user", "show"])
        .args(arguments);
    read_command(command).await
}

pub(crate) async fn read_command(command: std::process::Command) -> Result<String> {
    // Accepted bounded native inspection survives loss of its caller.
    tokio::spawn(async move {
        let mut command = tokio::process::Command::from(command);
        command
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        let mut child = command.spawn().map_err(|_| HelperError::Unavailable)?;
        let Some(stdout) = child.stdout.take() else {
            let _ = child.start_kill();
            let _ = child.wait().await;
            return Err(HelperError::Io);
        };
        let mut output = stdout.take(INSPECTION_BYTES as u64 + 1);
        let mut bytes = Vec::new();
        let collected = tokio::time::timeout(Duration::from_secs(3), async {
            let (read, status) = tokio::join!(output.read_to_end(&mut bytes), child.wait());
            read.map_err(|_| HelperError::Io)?;
            let status = status.map_err(|_| HelperError::Io)?;
            if !status.success() || bytes.len() > INSPECTION_BYTES {
                return Err(HelperError::Unavailable);
            }
            String::from_utf8(bytes).map_err(|_| HelperError::Invalid)
        })
        .await;
        if let Ok(result) = collected {
            result
        } else {
            let _ = child.start_kill();
            let _ = child.wait().await;
            Err(HelperError::Unavailable)
        }
    })
    .await
    .map_err(|_| HelperError::Io)?
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fmt::Write as _;
    fn fixture() -> (TransientUnit, String, String) {
        let unit = TransientUnit::new(&"a".repeat(32), 5).unwrap();
        let group = format!(
            "/user.slice/user-1000.slice/user@1000.service/app.slice/{}",
            unit.name()
        );
        let mut properties = String::new();
        for (name, _, value) in PROPERTIES {
            writeln!(properties, "{}={value}", queried_name(name)).unwrap();
        }
        writeln!(properties, "MainPID=51\nControlGroup={group}").unwrap();
        (unit, properties, format!("0::{group}\n"))
    }
    #[test]
    fn exact_lifecycle_and_process_membership_are_required_before_ready() {
        let (unit, properties, group) = fixture();
        validate_properties(&properties, &group, &unit, 51).unwrap();
        for (key, _, value) in PROPERTIES {
            assert!(
                validate_properties(
                    &properties.replace(
                        &format!("{}={value}", queried_name(key)),
                        &format!("{}=wrong", queried_name(key))
                    ),
                    &group,
                    &unit,
                    51
                )
                .is_err()
            );
        }
        for bad in [
            properties.replace("MainPID=51", "MainPID=52"),
            format!("{properties}MainPID=51\n"),
            properties.replace("ControlGroup=/user.slice/", "ControlGroup=/system.slice/"),
        ] {
            assert!(validate_properties(&bad, &group, &unit, 51).is_err());
        }
        assert!(
            validate_properties(&properties, &group.replace("0::", "1:cpu:"), &unit, 51).is_err()
        );
        assert!(
            validate_properties(
                &properties,
                &format!("{group}1:memory:/another\n"),
                &unit,
                51
            )
            .is_err()
        );
    }
    #[test]
    fn launcher_has_fixed_properties_no_environment_expansion_and_no_ambient_bus() {
        let unit = TransientUnit::new(&"b".repeat(32), 7).unwrap();
        let command = unit
            .command(Path::new("/private/helper with spaces"))
            .unwrap();
        let args = command
            .get_args()
            .map(|arg| arg.to_str().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(
            &args[..5],
            [
                "--user",
                "--pipe",
                "--wait",
                "--collect",
                "--expand-environment=no"
            ]
        );
        for (name, value, _) in PROPERTIES {
            assert!(args.contains(&format!("--property={name}={value}").as_str()));
        }
        assert_eq!(
            &args[args.len() - 2..],
            ["--", "/private/helper with spaces"]
        );
        let env = command.get_envs().collect::<Vec<_>>();
        assert_eq!(env.len(), 1);
        assert_eq!(env[0].0, "XDG_RUNTIME_DIR");
        for service in ["alias", "../bad", &"f".repeat(33), &"A".repeat(32)] {
            assert!(TransientUnit::new(service, 1).is_err());
        }
        assert!(TransientUnit::new(&"a".repeat(32), 0).is_err());
        assert!(unit.command(Path::new("relative")).is_err());
    }

    #[tokio::test(start_paused = true)]
    async fn only_fresh_connection_heartbeats_feed_the_watchdog() {
        let (socket, observer) = UnixDatagram::pair().unwrap();
        let watchdog = SystemdWatchdog { socket, epoch: 5 };
        let (left, right) = tokio::io::duplex(1024);
        let (lr, lw) = tokio::io::split(left);
        let (rr, rw) = tokio::io::split(right);
        let (client, _) = Connection::start(lr, lw, Role::Client, 5).unwrap();
        let (helper, mut incoming) = Connection::start(rr, rw, Role::Helper, 5).unwrap();
        let pump = tokio::spawn(watchdog.run(helper));
        let mut bytes = [0; 128];
        let length = observer.recv(&mut bytes).await.unwrap();
        assert_eq!(&bytes[..length], b"READY=1");
        client.heartbeat().await.unwrap();
        let length = observer.recv(&mut bytes).await.unwrap();
        assert_eq!(&bytes[..length], b"WATCHDOG=1");
        let connection = client.clone();
        let call = tokio::spawn(async move { connection.call(vec![1]).await });
        incoming.next().await.unwrap().reply(vec![2]).unwrap();
        call.await.unwrap().unwrap();
        assert!(
            tokio::time::timeout(Duration::from_mins(1), observer.recv(&mut bytes))
                .await
                .is_err(),
            "ordinary traffic and local time cannot feed the watchdog"
        );
        client.heartbeat().await.unwrap();
        let length = observer.recv(&mut bytes).await.unwrap();
        assert_eq!(&bytes[..length], b"WATCHDOG=1");
        client.close();
        pump.await.unwrap().unwrap();
    }

    #[tokio::test(start_paused = true)]
    async fn wrong_epoch_or_role_never_publishes_ready() {
        for (role, epoch) in [(Role::Helper, 6), (Role::Client, 5)] {
            let (socket, observer) = UnixDatagram::pair().unwrap();
            let watchdog = SystemdWatchdog { socket, epoch: 5 };
            let (left, _right) = tokio::io::duplex(128);
            let (read, write) = tokio::io::split(left);
            let (connection, _) = Connection::start(read, write, role, epoch).unwrap();
            assert_eq!(watchdog.run(connection).await, Err(HelperError::Invalid));
            let mut bytes = [0; 128];
            assert!(
                tokio::time::timeout(Duration::from_secs(1), observer.recv(&mut bytes))
                    .await
                    .is_err()
            );
        }
    }
}
