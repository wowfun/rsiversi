use crate::work::ApplicationWork;
use rsi_api_protocol::ApiError;
use std::io::Write;

pub(crate) async fn run(
    execute: impl std::future::Future<Output = rsi_api_protocol::Result<zeroize::Zeroizing<Vec<u8>>>>,
    work: ApplicationWork,
    delivery_error: &'static str,
) -> u8 {
    #[cfg(unix)]
    let diagnostics_stop = work.stop.clone();
    let result = async {
        let bytes = tokio::select! {
            biased;
            () = work.stop.cancelled() => return Err(ApiError::OutcomeUnknown),
            result = execute => result?,
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
            delivery.map_err(|_| ApiError::Backend(delivery_error.into()))
        })
        .await
        .map_err(|_| ApiError::Backend("operator output task failed".into()))?
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
    fn retiring_operator_output_interrupts_backpressure_and_restores_descriptor_flags() {
        let (writer, _unread) = std::os::unix::net::UnixStream::pair().unwrap();
        // Darwin records FWASWRITTEN in F_GETFL after the first write. Establish
        // that kernel history before comparing all descriptor flags exactly.
        // UnixStream::write uses send(), which does not set this write(2) bit.
        assert_eq!(rustix::io::write(&writer, b"x").unwrap(), 1);
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
