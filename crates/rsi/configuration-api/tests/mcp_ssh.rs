use rsi_api_protocol::{ApiError, HostEpoch};
use rsi_configuration_api::mcp_ssh::{Change, State, Target};
use rsi_mcp_protocol::{EnvironmentValue, ServerConfig, TransportConfig};
#[test]
fn target_stdio_rejects_foreign_replies_local_transport_and_account_environment_overrides() {
    let epoch = HostEpoch::from_bytes([1; 16]);
    let target = Target {
        host_epoch: epoch.clone(),
        target: rsi_execution_protocol::ExecutionTargetId::parse("a".repeat(32)).unwrap(),
        server: "fixture".into(),
    };
    let config = ServerConfig {
        id: "fixture".into(),
        enabled: true,
        tools: vec![],
        resource_templates: false,
        transport: TransportConfig::SshStdio {
            target: target.target.clone(),
            command: "python3".into(),
            arguments: vec![],
            cwd: "/target/project".into(),
            environment: std::collections::BTreeMap::new(),
        },
    };
    let input = Change {
        target: target.clone(),
        expected: "0".into(),
        config: Some(config.clone()),
    };
    input.validate(&epoch, true).unwrap();
    assert!(input.validate(&epoch, false).is_err());
    assert!(
        input
            .validate(&HostEpoch::from_bytes([2; 16]), true)
            .is_err()
    );
    for key in [
        "HOME",
        "PATH",
        "SSH_AUTH_SOCK",
        "LD_PRELOAD",
        "NOTIFY_SOCKET",
        "WATCHDOG_PID",
        "DBUS_SESSION_BUS_ADDRESS",
    ] {
        let mut malicious = input.clone();
        let TransportConfig::SshStdio { environment, .. } =
            &mut malicious.config.as_mut().unwrap().transport
        else {
            panic!()
        };
        environment.insert(
            key.into(),
            EnvironmentValue::Credential {
                reference: rsi_credentials_protocol::CredentialRef::new("rsi.mcp", "allowed")
                    .unwrap(),
            },
        );
        assert!(matches!(
            malicious.validate(&epoch, true),
            Err(ApiError::Invalid(_))
        ));
    }
    let mut reply = State {
        target: target.clone(),
        revision: "1".into(),
        config: Some(config),
        apply_error: None,
    };
    reply.validate(&target).unwrap();
    reply.target.target = rsi_execution_protocol::ExecutionTargetId::parse("b".repeat(32)).unwrap();
    assert!(reply.validate(&target).is_err());
    reply.target = target.clone();
    reply.config.as_mut().unwrap().transport = TransportConfig::Stdio {
        program: "/Service/program".into(),
        arguments: vec![],
        cwd: "/Service/workspace".into(),
        environment: std::collections::BTreeMap::new(),
    };
    assert!(reply.validate(&target).is_err());
}
