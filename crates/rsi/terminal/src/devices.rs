use crate::{RsiError, work::ApplicationWork};
use rsi_api_device_api::DeviceClient;
use rsi_api_protocol::{ApiClient, ApiError, DeviceId, DeviceRecord};
use serde::Serialize;
use std::{ffi::OsString, io::Write, sync::Arc};

/// Terminal help for the Session-independent device administration application.
pub const HELP: &str = "Devices application:\n  rsi --profile devices register LABEL\n  rsi --profile devices list\n  rsi --profile devices revoke DEVICE_ID\nRegistration prints its one-time token; list prints only non-secret records.\n";

#[derive(Debug)]
pub(crate) enum Command {
    Register(String),
    List,
    Revoke(DeviceId),
}
impl Command {
    pub fn parse(arguments: &[OsString]) -> crate::Result<Self> {
        let arguments = arguments
            .iter()
            .map(|value| {
                value
                    .to_str()
                    .ok_or_else(|| RsiError::Boot("device arguments must be UTF-8".into()))
            })
            .collect::<crate::Result<Vec<_>>>()?;
        match arguments.as_slice() {
            ["list"] => Ok(Self::List),
            ["register", label] => {
                DeviceRecord::validate_label(label)
                    .map_err(|error| RsiError::Boot(error.to_string()))?;
                Ok(Self::Register((*label).into()))
            }
            ["revoke", id] => Ok(Self::Revoke(
                DeviceId::parse(*id).map_err(|error| RsiError::Boot(error.to_string()))?,
            )),
            _ => Err(RsiError::Boot(HELP.into())),
        }
    }
    async fn execute(
        self,
        client: &DeviceClient,
    ) -> rsi_api_protocol::Result<zeroize::Zeroizing<Vec<u8>>> {
        let encoded = match self {
            Self::Register(label) => {
                #[derive(Serialize)]
                struct Receipt<'a> {
                    endpoint_id: &'a rsi_api_protocol::EndpointId,
                    id: &'a DeviceId,
                    label: &'a str,
                    token: &'a str,
                }
                let issued = client.register(&label).await?;
                serde_json::to_vec(&Receipt {
                    endpoint_id: client.endpoint_id(),
                    id: &issued.record.id,
                    label: &issued.record.label,
                    token: issued.token.expose_secret(),
                })
            }
            Self::List => serde_json::to_vec(&client.list().await?),
            Self::Revoke(id) => serde_json::to_vec(&client.revoke(&id).await?),
        }
        .map_err(|_| ApiError::Backend("cannot encode device result".into()))?;
        Ok(zeroize::Zeroizing::new(encoded))
    }
}

pub(crate) async fn run(api: Arc<dyn ApiClient>, command: Command, work: ApplicationWork) -> u8 {
    #[cfg(unix)]
    let diagnostics_stop = work.stop.clone();
    let result = async {
        let client = DeviceClient::new(api)?;
        let bytes = tokio::select! {
            biased;
            () = work.stop.cancelled() => return Err(ApiError::OutcomeUnknown),
            result = command.execute(&client) => result?,
        };
        let token = work.tasks.token();
        tokio::task::spawn_blocking(move || {
            let _token = token;
            #[cfg(unix)]
            let delivery = write_to(std::io::stdout(), &bytes, work.stop);
            #[cfg(not(unix))]
            let delivery = {
                let mut output = std::io::stdout().lock();
                output
                    .write_all(&bytes)
                    .and_then(|()| output.write_all(b"\n"))
                    .and_then(|()| output.flush())
            };
            delivery.map_err(|_| {
                ApiError::Backend("device output delivery failed; reconcile with list".into())
            })
        })
        .await
        .map_err(|_| ApiError::Backend("device output task failed".into()))?
    }
    .await;
    match result {
        Ok(()) => 0,
        Err(error) => {
            #[cfg(unix)]
            if let Ok(mut output) = crate::work::Output::new(std::io::stderr(), diagnostics_stop) {
                let _ = writeln!(output, "error: {error}");
            }
            #[cfg(not(unix))]
            eprintln!("error: {error}");
            1
        }
    }
}

#[cfg(unix)]
fn write_to(
    fd: impl std::os::fd::AsFd,
    bytes: &[u8],
    stop: tokio_util::sync::CancellationToken,
) -> std::io::Result<()> {
    let mut output = crate::work::Output::new(fd, stop)?;
    output.write_all(bytes)?;
    output.write_all(b"\n")?;
    output.flush()
}

#[cfg(all(test, unix))]
mod tests {
    #[test]
    fn retiring_device_output_interrupts_backpressure_and_restores_descriptor_flags() {
        let (mut writer, _unread) = std::os::unix::net::UnixStream::pair().unwrap();
        // Darwin records FWASWRITTEN in F_GETFL after the first write. Establish
        // that kernel history before comparing all descriptor flags exactly.
        std::io::Write::write_all(&mut writer, b"x").unwrap();
        let original = rustix::fs::fcntl_getfl(&writer).unwrap();
        let worker_fd = writer.try_clone().unwrap();
        let stop = tokio_util::sync::CancellationToken::new();
        let worker_stop = stop.clone();
        let (done, result) = std::sync::mpsc::channel();
        let thread = std::thread::spawn(move || {
            done.send(super::write_to(
                worker_fd,
                &vec![b'x'; 4 * 1024 * 1024],
                worker_stop,
            ))
            .unwrap();
        });
        let initial = result.recv_timeout(std::time::Duration::from_millis(50));
        stop.cancel();
        assert!(
            matches!(initial, Err(std::sync::mpsc::RecvTimeoutError::Timeout)),
            "writer completed before cancellation: {initial:?}"
        );
        assert_eq!(
            result
                .recv_timeout(std::time::Duration::from_secs(1))
                .unwrap()
                .unwrap_err()
                .kind(),
            std::io::ErrorKind::BrokenPipe
        );
        thread.join().unwrap();
        assert_eq!(rustix::fs::fcntl_getfl(&writer).unwrap(), original);
    }
}
