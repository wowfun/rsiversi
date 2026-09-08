#![cfg(unix)]

use rsi_api_protocol::{EndpointId, HostEpoch, LocalCompatibilityKey};
use rsi_api_uds_client::UdsClientConfig;
use std::{os::unix::net::SocketAddr, path::PathBuf};

#[test]
fn client_socket_validation_matches_the_native_address_boundary() {
    let mut config = UdsClientConfig {
        socket: PathBuf::new(),
        endpoint_id: EndpointId::generate().unwrap(),
        host_epoch: HostEpoch::generate().unwrap(),
        compatibility: LocalCompatibilityKey::parse("a".repeat(64)).unwrap(),
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
        config.socket = PathBuf::from(path);
        let expected =
            config.socket.is_absolute() && SocketAddr::from_pathname(&config.socket).is_ok();
        assert_eq!(
            config.validate().is_ok(),
            expected,
            "path={:?}",
            config.socket
        );
    }
}
