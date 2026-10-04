use crate::{
    ArtifactCache, ArtifactLease, CacheError, ExecutionServer, HelperError, Result,
    SystemdWatchdog, TransientUnit, WriterLockPolicy, lifecycle::runtime_directory,
    native::NativeRuntime, stdio::Port,
};
use rsi_ssh_protocol::rpc::{Reply, Request};
use rsi_ssh_transport::{Connection, Incoming, RequestKind, Role};
use std::{
    ffi::OsString,
    time::{Duration, Instant},
};

fn cache_error(error: CacheError) -> HelperError {
    match error {
        CacheError::ContentionTimeout => HelperError::CacheContentionTimeout,
        _ => HelperError::Unavailable,
    }
}

/// Closed helper entry selection; all coordinates are checked before native work.
#[derive(Debug)]
pub struct Invocation {
    mode: Mode,
    service: String,
    epoch: u64,
    digest: [u8; 32],
}
#[derive(Debug)]
enum Mode {
    InstallLaunch,
    Serve,
}
impl Invocation {
    /// Accepts exactly mode, Service namespace, decimal epoch and lowercase SHA-256.
    pub fn parse(arguments: &[OsString]) -> Result<Self> {
        let [mode, service, epoch, digest] = arguments else {
            return Err(HelperError::Invalid);
        };
        let mode = match mode.to_str() {
            Some("install-launch") => Mode::InstallLaunch,
            Some("serve") => Mode::Serve,
            _ => return Err(HelperError::Invalid),
        };
        let service = service
            .to_str()
            .filter(|value| crate::valid_service_namespace(value))
            .ok_or(HelperError::Invalid)?
            .to_owned();
        let epoch = epoch
            .to_str()
            .filter(|value| {
                !value.is_empty()
                    && value.len() <= 20
                    && value.bytes().all(|byte| byte.is_ascii_digit())
            })
            .and_then(|value| value.parse::<u64>().ok())
            .filter(|value| *value != 0)
            .ok_or(HelperError::Invalid)?;
        let digest = digest
            .to_str()
            .filter(|value| {
                value.len() == 64
                    && value
                        .bytes()
                        .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
            })
            .ok_or(HelperError::Invalid)?;
        let digest = hex::decode(digest)
            .map_err(|_| HelperError::Invalid)?
            .try_into()
            .map_err(|_| HelperError::Invalid)?;
        Ok(Self {
            mode,
            service,
            epoch,
            digest,
        })
    }
    /// Runs the selected native entry. Reusable orchestration stays outside the app.
    pub async fn run(self) -> Result<()> {
        match self.mode {
            Mode::InstallLaunch => self.install_launch().await,
            Mode::Serve => self.serve().await,
        }
    }
    async fn install_launch(self) -> Result<()> {
        let service = self.service.clone();
        let digest = self.digest;
        let lease = tokio::task::spawn_blocking(move || {
            let cache = ArtifactCache::open(&runtime_directory(), &service)
                .map_err(|_| HelperError::Unavailable)?;
            let mut image = std::fs::File::open("/proc/self/exe").map_err(|_| HelperError::Io)?;
            cache
                .publish(
                    digest,
                    &mut image,
                    WriterLockPolicy::WaitUntil(Instant::now() + Duration::from_secs(2)),
                )
                .map_err(cache_error)
        })
        .await
        .map_err(|_| HelperError::Io)??;
        let unit = TransientUnit::new(&self.service, self.epoch)?;
        let mut command = unit.command(&lease.executable())?;
        command.args([
            "serve",
            &self.service,
            &self.epoch.to_string(),
            &hex::encode(self.digest),
        ]);
        // The retained launcher owns its lease and actual child through wait.
        // Its sole byte-port owner is the unit; no launcher task reads stdin.
        tokio::spawn(async move {
            let _lease = lease;
            let mut child = tokio::process::Command::from(command)
                .spawn()
                .map_err(|_| HelperError::Unavailable)?;
            let status = child.wait().await.map_err(|_| HelperError::Io)?;
            if status.success() {
                Ok(())
            } else if status.code() == Some(i32::from(rsi_ssh_protocol::CACHE_CONTENTION_EXIT_CODE))
            {
                Err(HelperError::CacheContentionTimeout)
            } else {
                Err(HelperError::Unavailable)
            }
        })
        .await
        .map_err(|_| HelperError::Io)?
    }
    async fn serve(self) -> Result<()> {
        let service = self.service.clone();
        let digest = self.digest;
        let lease: ArtifactLease = tokio::task::spawn_blocking(move || {
            ArtifactCache::open(&runtime_directory(), &service).and_then(|cache| {
                cache.acquire(
                    digest,
                    WriterLockPolicy::WaitUntil(Instant::now() + Duration::from_secs(2)),
                )
            })
        })
        .await
        .map_err(|_| HelperError::Io)?
        .map_err(cache_error)?;
        let unit = TransientUnit::new(&self.service, self.epoch)?;
        let watchdog = SystemdWatchdog::verify(&unit).await?;
        let native = NativeRuntime::new().await?;
        let result = self.run_connection(&native, watchdog, &lease).await;
        let cleanup = native.close().await;
        result.and(cleanup)
    }
    async fn run_connection(
        &self,
        native: &NativeRuntime,
        watchdog: SystemdWatchdog,
        lease: &ArtifactLease,
    ) -> Result<()> {
        let (connection, mut incoming) = Connection::start(
            Port::input().map_err(|_| HelperError::Io)?,
            Port::output().map_err(|_| HelperError::Io)?,
            Role::Helper,
            self.epoch,
        )
        .map_err(|_| HelperError::Io)?;
        let mut watchdog = tokio::spawn(watchdog.run(connection.clone()));
        let initialized = tokio::time::timeout(
            Duration::from_secs(10),
            initialize(native, lease, &mut incoming),
        )
        .await
        .unwrap_or(Err(HelperError::Unavailable));
        let result = match initialized {
            Ok(server) => {
                let serve = server.serve(connection.clone(), incoming);
                tokio::pin!(serve);
                tokio::select! { biased;
                    result = &mut watchdog => {
                        connection.close();
                        let _ = serve.await;
                        result.map_err(|_| HelperError::Io)?
                    }
                    result = &mut serve => result.map_err(|_| HelperError::Io),
                }
            }
            Err(error) => Err(error),
        };
        connection.close();
        connection.settled().await;
        if !watchdog.is_finished() {
            let _ = watchdog.await;
        }
        result
    }
}
async fn initialize(
    native: &NativeRuntime,
    lease: &ArtifactLease,
    incoming: &mut Incoming,
) -> Result<ExecutionServer> {
    let request = incoming.next().await.ok_or(HelperError::Unavailable)?;
    if request.kind() != RequestKind::Ordinary {
        return Err(HelperError::Invalid);
    }
    let Request::Initialize { configuration } =
        serde_json::from_slice(request.payload()).map_err(|_| HelperError::Invalid)?
    else {
        return Err(HelperError::Invalid);
    };
    let (programs, unavailable) = crate::native::programs(configuration).await?;
    let server = ExecutionServer::new(native.capabilities(), programs)
        .await
        .map_err(|_| HelperError::Unavailable)?;
    let ready = Reply::Ready {
        artifact: hex::encode(lease.digest()),
        unavailable,
    };
    request
        .reply(serde_json::to_vec(&ready).map_err(|_| HelperError::Invalid)?)
        .map_err(|_| HelperError::Io)?;
    Ok(server)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn entry_rejects_unbounded_extra_and_ambiguous_coordinates_before_native_work() {
        let args = vec![
            OsString::from("serve"),
            OsString::from("a".repeat(32)),
            OsString::from("1"),
            OsString::from("b".repeat(64)),
        ];
        Invocation::parse(&args).unwrap();
        for (index, invalid) in [
            (0, "--shell"),
            (1, "../../outside"),
            (2, "0"),
            (2, "+1"),
            (3, "ABC"),
        ] {
            let mut bad = args.clone();
            bad[index] = invalid.into();
            assert!(Invocation::parse(&bad).is_err());
        }
        let mut extra = args;
        extra.push("ignored".into());
        assert!(Invocation::parse(&extra).is_err());
    }
}
