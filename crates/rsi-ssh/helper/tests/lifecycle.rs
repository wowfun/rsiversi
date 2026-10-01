#![cfg(target_os = "linux")]

use rsi_ssh_helper::{SystemdWatchdog, TransientUnit};
use rsi_ssh_transport::{Connection, Role};
use std::{
    io::Write,
    path::Path,
    process::Stdio,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, BufReader};

const SERVICE: &str = "77ae5f6b52d44840acb509fc6b2f5eca";

// Invoked only as the main process of the isolated transient test service.
#[test]
#[ignore = "entry fixture for native_watchdog_and_cgroup_cleanup"]
fn systemd_child_fixture() {
    let name = std::env::var("RSI_TEST_UNIT").expect("fixture requires its isolated systemd unit");
    let epoch = u64::from_str_radix(
        name.strip_suffix(".service")
            .unwrap()
            .rsplit('-')
            .next()
            .unwrap(),
        16,
    )
    .unwrap();
    let unit = TransientUnit::new(SERVICE, epoch).unwrap();
    assert_eq!(unit.name(), name);
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            let watchdog = SystemdWatchdog::verify(&unit)
                .await
                .expect("exact systemd lifecycle verification");
            let descendant = std::process::Command::new("/usr/bin/setsid")
                .env_clear()
                .args(["/bin/sh", "-c", "trap '' TERM; exec /bin/sleep 300"])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap();
            println!(
                "RSI_LIFECYCLE_READY {} {}",
                std::process::id(),
                descendant.id()
            );
            std::io::stdout().flush().unwrap();
            let (connection, mut requests) =
                Connection::start(tokio::io::stdin(), tokio::io::stdout(), Role::Helper, epoch)
                    .unwrap();
            let pump = tokio::spawn(watchdog.run(connection.clone()));
            while let Some(request) = requests.next().await {
                assert_eq!(request.payload(), b"fixture-round-trip");
                request.reply(b"verified".to_vec()).unwrap();
            }
            pump.await.unwrap().unwrap();
            connection.close();
            connection.settled().await;
            // systemd, rather than a process-group signal, owns this setsid descendant.
            drop(descendant);
        });
}

struct UnitCleanup(String);
impl Drop for UnitCleanup {
    fn drop(&mut self) {
        let _ = std::process::Command::new("/usr/bin/systemctl")
            .args(["--user", "stop", "--", &self.0])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
}

struct NativeFixture {
    cleanup: UnitCleanup,
    child: tokio::process::Child,
    diagnostics: tokio::task::JoinHandle<String>,
    connection: Connection,
    main: i32,
    descendant: i32,
}

async fn launch_fixture() -> NativeFixture {
    let epoch = u64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos(),
    )
    .unwrap();
    let unit = TransientUnit::new(SERVICE, epoch).unwrap();
    let cleanup = UnitCleanup(unit.name().into());
    let mut launch = unit.command(Path::new("/usr/bin/env")).unwrap();
    launch
        .arg(format!("RSI_TEST_UNIT={}", unit.name()))
        .arg(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "systemd_child_fixture",
            "--ignored",
            "--nocapture",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut launch = tokio::process::Command::from(launch);
    launch.kill_on_drop(true);
    let mut child = launch.spawn().unwrap();
    let stderr = child.stderr.take().unwrap();
    let diagnostics = tokio::spawn(async move {
        let mut bytes = Vec::new();
        stderr
            .take(16 * 1024)
            .read_to_end(&mut bytes)
            .await
            .unwrap();
        String::from_utf8_lossy(&bytes).into_owned()
    });
    let mut reader = BufReader::new(child.stdout.take().unwrap());
    let marker = tokio::time::timeout(Duration::from_secs(9), async {
        let mut read = 0;
        loop {
            let mut line = String::new();
            let bytes = (&mut reader).take(1025).read_line(&mut line).await.unwrap();
            read += bytes;
            assert!(
                bytes > 0 && read <= 4096,
                "helper fixture exited before its ready marker"
            );
            if let Some((_, values)) = line.split_once("RSI_LIFECYCLE_READY ") {
                let fields = values
                    .split_whitespace()
                    .map(|value| value.parse::<i32>().unwrap())
                    .collect::<Vec<_>>();
                assert_eq!(fields.len(), 2);
                return (fields[0], fields[1]);
            }
        }
    })
    .await;
    let (main, descendant) = match marker {
        Ok(ids) => ids,
        Err(error) => {
            let _ = child.start_kill();
            let _ = child.wait().await;
            panic!("fixture startup: {error}; {}", diagnostics.await.unwrap());
        }
    };
    let (connection, _) =
        Connection::start(reader, child.stdin.take().unwrap(), Role::Client, epoch).unwrap();
    NativeFixture {
        cleanup,
        child,
        diagnostics,
        connection,
        main,
        descendant,
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires the native Linux user systemd manager; exercises the actual 30-second watchdog"]
async fn native_watchdog_and_cgroup_cleanup() {
    let NativeFixture {
        cleanup: _cleanup,
        mut child,
        diagnostics,
        connection,
        main,
        descendant,
    } = launch_fixture().await;
    tokio::time::timeout(Duration::from_secs(2), connection.heartbeat())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        tokio::time::timeout(
            Duration::from_secs(2),
            connection.call(b"fixture-round-trip".to_vec())
        )
        .await
        .unwrap()
        .unwrap()
        .as_bytes(),
        b"verified"
    );
    let stat = std::fs::read_to_string(format!("/proc/{descendant}/stat")).unwrap();
    let fields = stat
        .rsplit_once(')')
        .unwrap()
        .1
        .split_whitespace()
        .collect::<Vec<_>>();
    assert_eq!(
        fields[3].parse::<i32>().unwrap(),
        descendant,
        "descendant must have escaped the helper's session"
    );
    eprintln!("native helper ready; real stdio RPC and setsid descendant verified");

    // Run longer than one real watchdog interval; only validated peer heartbeats
    // keep the helper alive. No injected clock or shortened systemd property.
    for _ in 0..17 {
        tokio::time::sleep(Duration::from_secs(2)).await;
        tokio::time::timeout(Duration::from_secs(2), connection.heartbeat())
            .await
            .unwrap()
            .unwrap();
        assert!(child.try_wait().unwrap().is_none());
    }
    eprintln!("fresh transport heartbeats kept the unit alive beyond 30 seconds");
    rustix::process::kill_process(
        rustix::process::Pid::from_raw(main).unwrap(),
        rustix::process::Signal::STOP,
    )
    .unwrap();
    let started = tokio::time::Instant::now();
    let status = tokio::time::timeout(Duration::from_secs(40), child.wait())
        .await
        .unwrap()
        .unwrap();
    assert!(!status.success());
    assert!(
        started.elapsed() >= Duration::from_secs(25),
        "watchdog must retain its configured 30-second interval"
    );
    connection.closed().await;
    connection.settled().await;
    let diagnostics = diagnostics.await.unwrap();
    assert!(
        diagnostics.contains("watchdog"),
        "systemd result did not identify watchdog expiry: {diagnostics}"
    );
    tokio::time::timeout(Duration::from_secs(5), async {
        while Path::new(&format!("/proc/{main}")).exists()
            || Path::new(&format!("/proc/{descendant}")).exists()
        {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("systemd must reap the stopped helper and TERM-ignoring setsid descendant");
    eprintln!(
        "watchdog expiry reaped both main and setsid descendant; elapsed={}ms",
        started.elapsed().as_millis()
    );
}
