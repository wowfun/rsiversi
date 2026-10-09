use super::*;
use rsi_ssh_protocol::rpc::Failure;
use rsi_ssh_transport::{Incoming, RequestKind};
use std::time::Duration;
use tokio::time::Instant;

fn pair() -> (ProcessConnection, Connection, Incoming) {
    let (left, right) = tokio::io::duplex(1024);
    let (lr, lw) = tokio::io::split(left);
    let (rr, rw) = tokio::io::split(right);
    let (client, _) = Connection::start(lr, lw, Role::Client, 37).unwrap();
    let (helper, incoming) = Connection::start(rr, rw, Role::Helper, 37).unwrap();
    (ProcessConnection::new(client).unwrap(), helper, incoming)
}
#[cfg(unix)]
#[tokio::test]
async fn closed_files_transport_reports_retirement_instead_of_token_loss() {
    use rsi_files_protocol::{
        FileKind, FileToken, Files, FilesBinding, FilesCaller, FilesError, OpenedFile, RelativePath,
    };
    let (client, helper, mut incoming) = pair();
    let files = RemoteFiles::new(client.clone());
    let root = tempfile::tempdir().unwrap();
    let binding = FilesBinding::new(
        FilesCaller::default(),
        "subject",
        "revision",
        root.path().canonicalize().unwrap(),
    )
    .unwrap();
    let opened = OpenedFile {
        token: FileToken::try_from("a".repeat(32)).unwrap(),
        path: RelativePath::new(b"object").unwrap(),
        kind: FileKind::File,
        executable: false,
        length: 1,
    };
    let replying = tokio::spawn({
        let opened = opened.clone();
        async move {
            incoming
                .next()
                .await
                .unwrap()
                .reply(serde_json::to_vec(&Reply::FilesOpened { handle: 1, opened }).unwrap())
                .unwrap();
        }
    });
    assert_eq!(
        files
            .open(
                binding.clone(),
                opened.path.clone(),
                opened.kind,
                tokio_util::sync::CancellationToken::new()
            )
            .await
            .unwrap(),
        opened
    );
    replying.await.unwrap();
    assert_eq!(
        files.describe(&binding, &FileToken::try_from("b".repeat(32)).unwrap()),
        Err(FilesError::Unavailable)
    );
    client.transport.close();
    assert_eq!(
        files.describe(&binding, &opened.token),
        Err(FilesError::Cancelled)
    );
    assert_eq!(
        files
            .read(
                binding.clone(),
                opened.token.clone(),
                0,
                1,
                tokio_util::sync::CancellationToken::new()
            )
            .await,
        Err(FilesError::Cancelled)
    );
    assert_eq!(
        files
            .list(
                binding,
                opened.token,
                0,
                1,
                tokio_util::sync::CancellationToken::new()
            )
            .await,
        Err(FilesError::Cancelled)
    );
    helper.close();
}

#[tokio::test(start_paused = true)]
async fn termination_capacity_backs_off_then_stops_after_acknowledgement() {
    let (client, helper, mut incoming) = pair();
    let replying = tokio::spawn(async move {
        let mut times = Vec::new();
        for index in 0..9 {
            let request = incoming.next().await.unwrap();
            assert_eq!(
                request.kind(),
                RequestKind::Control(Control::Terminate { process: 42 })
            );
            times.push(Instant::now());
            request
                .reply(
                    serde_json::to_vec(&if index < 8 {
                        Reply::Failed {
                            failure: Failure::Capacity,
                        }
                    } else {
                        Reply::Done
                    })
                    .unwrap(),
                )
                .unwrap();
        }
        times
    });
    client.cleanup_terminate(42).await.unwrap();
    let times = replying.await.unwrap();
    for (pair, delay) in times.windows(2).zip([1, 2, 4, 8, 16, 32, 64, 100]) {
        let actual = pair[1] - pair[0];
        assert!(
            actual >= Duration::from_millis(delay) && actual <= Duration::from_millis(delay + 1),
            "expected {delay} ms capacity backoff, got {actual:?}"
        );
    }
    assert!(!client.is_closed());
    client.transport.close();
    helper.close();
}

#[tokio::test(start_paused = true)]
async fn termination_saturation_keeps_the_original_deadline_and_retires_epoch() {
    let (client, helper, mut incoming) = pair();
    let replying = tokio::spawn(async move {
        let mut requests = 0;
        while let Some(request) = incoming.next().await {
            requests += 1;
            if request
                .reply(
                    serde_json::to_vec(&Reply::Failed {
                        failure: Failure::Capacity,
                    })
                    .unwrap(),
                )
                .is_err()
            {
                break;
            }
        }
        requests
    });
    let started = Instant::now();
    assert!(matches!(
        client.cleanup_terminate(42).await,
        Err(ProcessError::OutcomeUnknown)
    ));
    assert_eq!(started.elapsed(), Duration::from_secs(30));
    assert!(client.is_closed());
    let requests = replying.await.unwrap();
    eprintln!("termination saturation: {requests} requests in 30 virtual seconds");
    assert!(
        requests <= 310,
        "capacity pressure must not busy-poll: {requests}"
    );
    helper.close();
}
