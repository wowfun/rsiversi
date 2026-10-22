use super::*;
use rsi_web_control::{Assets, Command as WebCommand, Reply};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn desktop_origin_survives_window_and_host_restart_and_rejects_invalid_identity() {
    let fixture = CliFixture::new("http://127.0.0.1:9");
    install_web(&fixture);
    fixture.assert_success(&["host", "start"]);
    let state = fixture.temporary.path().join("state/rsi");
    let owner = || -> rsi_service_host::HostOwnerMetadata {
        serde_json::from_slice(&std::fs::read(state.join("session-host/owner.json")).unwrap())
            .unwrap()
    };
    let open = async || {
        rsi_serve::request_listener(
            &owner(),
            WebCommand::Desktop {
                assets: Assets::default(),
            },
        )
        .await
    };
    // An interrupted first publication leaves only this bounded private staging name.
    std::fs::write(state.join("desktop-port.pending"), [12]).unwrap();
    let (first, reply) = open().await.unwrap();
    let Reply::Ready { address, .. } = reply else {
        panic!("Desktop readiness");
    };
    let record = state.join("desktop-port");
    assert_eq!(
        std::fs::read(&record).unwrap(),
        address.port().to_be_bytes()
    );
    assert!(!state.join("desktop-port.pending").exists());
    assert!(
        open().await.is_err(),
        "a second Desktop cannot change Origin"
    );
    drop(first);
    retired(address).await;

    let (second, reply) = open().await.unwrap();
    assert!(matches!(reply, Reply::Ready { address: actual, .. } if actual == address));
    drop(second);
    retired(address).await;
    fixture.assert_success(&["host", "stop"]);
    fixture.assert_success(&["host", "start"]);
    let (third, reply) = open().await.unwrap();
    assert!(matches!(reply, Reply::Ready { address: actual, .. } if actual == address));
    drop(third);
    retired(address).await;

    let blocker = tokio::net::TcpListener::bind(address).await.unwrap();
    let Err(error) = open().await else {
        panic!("occupied Desktop port must fail without allocating another Origin");
    };
    assert!(error.contains(&format!("port {}", address.port())));
    drop(blocker);
    assert_eq!(
        std::fs::read(&record).unwrap(),
        address.port().to_be_bytes()
    );

    for bytes in [&[][..], &[0][..], &[0, 0][..], &[1, 2, 3][..]] {
        std::fs::write(&record, bytes).unwrap();
        let Err(error) = open().await else {
            panic!("malformed persistent port accepted");
        };
        assert!(error.contains("desktop-port"));
        assert_eq!(std::fs::read(&record).unwrap(), bytes);
    }
    std::fs::remove_file(&record).unwrap();
    #[cfg(unix)]
    {
        let unrelated = state.join("unrelated-port");
        std::fs::write(&unrelated, address.port().to_be_bytes()).unwrap();
        std::os::unix::fs::symlink(&unrelated, &record).unwrap();
        assert!(open().await.is_err(), "durable record symlinks must reject");
        assert_eq!(
            std::fs::read(&unrelated).unwrap(),
            address.port().to_be_bytes()
        );
        std::fs::remove_file(&record).unwrap();
    }
    std::fs::create_dir(&record).unwrap();
    assert!(
        open().await.is_err(),
        "nonregular durable records must reject"
    );
    std::fs::remove_dir(&record).unwrap();
    std::fs::write(&record, address.port().to_be_bytes()).unwrap();
    // Recovery must preserve unknown/nonregular entries rather than deleting them.
    std::fs::create_dir(state.join("desktop-port.pending")).unwrap();
    assert!(open().await.is_err());
    assert!(state.join("desktop-port.pending").is_dir());
    std::fs::remove_dir(state.join("desktop-port.pending")).unwrap();
    let (last, reply) = open().await.unwrap();
    assert!(matches!(reply, Reply::Ready { address: actual, .. } if actual == address));
    drop(last);
    retired(address).await;
    fixture.assert_success(&["host", "status"]);
}

async fn retired(address: std::net::SocketAddr) {
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        while tokio::net::TcpStream::connect(address).await.is_ok() {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("connection loss must retire the Desktop listener");
}
