#![cfg(unix)]

use rsi_api_protocol::HostEpoch;
use rsi_service_host::{HostOwnerMetadata, HostOwnerMode};
use std::{os::unix::net::SocketAddr, path::PathBuf};

#[test]
fn durable_owner_socket_validation_matches_the_native_address_boundary() {
    let mut metadata = HostOwnerMetadata {
        format: 1,
        pid: 1,
        process_start_token: "fixture".into(),
        mode: HostOwnerMode::Daemon,
        launch_key: "a".repeat(64),
        protocol_epoch: 1,
        product_build: format!("fixture+sha256:{}", "b".repeat(64)),
        host_epoch: HostEpoch::generate().unwrap(),
        endpoint_id: None,
        socket_path: None,
    };
    for path in [
        "/tmp/a\0b".into(),
        "relative.sock".into(),
        "/tmp/valid.sock".into(),
        format!("/{}", "a".repeat(102)),
        format!("/{}", "a".repeat(103)),
        format!("/{}", "a".repeat(106)),
        format!("/{}", "a".repeat(107)),
    ] {
        let path = PathBuf::from(path);
        let expected = path.is_absolute() && SocketAddr::from_pathname(&path).is_ok();
        metadata.socket_path = Some(path.clone());
        assert_eq!(metadata.validate().is_ok(), expected, "path={path:?}");
    }
}
