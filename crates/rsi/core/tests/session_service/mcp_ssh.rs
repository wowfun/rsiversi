use super::super::{RunningRsi, composition, fixture, host_profile};
use super::{grant, raw};
use rsi_api_protocol::{ApiError, CallOrigin};
use rsi_configuration_api::{
    leaf::{Grant, GrantScope, Principal},
    mcp_ssh as wire,
};
use rsi_credentials_protocol::CredentialRef;
use rsi_mcp::{EnvironmentValue, ServerConfig, TransportConfig};
use serde_json::json;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[expect(
    clippy::too_many_lines,
    reason = "one public durable configuration, scoped credential, revocation and cold restore scenario"
)]
async fn ssh_stdio_management_requires_one_exact_credential_grant_and_live_use_to_refresh() {
    let fixture = fixture("http://127.0.0.1:1");
    let running =
        RunningRsi::boot_host_profile(composition(fixture.paths.clone()), &host_profile(&fixture))
            .await
            .unwrap();
    let device = running
        .device_administration()
        .unwrap()
        .register("mcp-target-device")
        .await
        .unwrap();
    let origin = CallOrigin::Device(
        running
            .device_authentication()
            .unwrap()
            .authenticate(&device.token)
            .unwrap(),
    );
    let mut target = wire::Target {
        host_epoch: running.connection_description().unwrap().host_epoch.clone(),
        target: rsi_execution::ExecutionTargetId::parse("a".repeat(32)).unwrap(),
        server: "target-peer".into(),
    };
    let refs = [
        CredentialRef::new("rsi.mcp", "allowed-a").unwrap(),
        CredentialRef::new("rsi.mcp", "allowed-b").unwrap(),
    ];
    let config = ServerConfig {
        id: target.server.clone(),
        enabled: true,
        tools: vec!["echo".into()],
        resource_templates: false,
        transport: TransportConfig::SshStdio {
            target: target.target.clone(),
            command: "python3".into(),
            arguments: vec![],
            cwd: "/target/workspace".into(),
            environment: [
                (
                    "TOKEN_A".into(),
                    EnvironmentValue::Credential {
                        reference: refs[0].clone(),
                    },
                ),
                (
                    "TOKEN_B".into(),
                    EnvironmentValue::Credential {
                        reference: refs[1].clone(),
                    },
                ),
            ]
            .into(),
        },
    };
    let mut put = wire::Change {
        target: target.clone(),
        expected: "0".into(),
        config: Some(config.clone()),
    };
    raw(
        &running,
        CallOrigin::Local,
        rsi_configuration_api::ConfigurationOperation::SetGrant.spec(),
        &json!({"device":device.record.id,"expected_revision":"0","granted":true}),
    )
    .await
    .unwrap();
    for operation in [wire::Operation::Put, wire::Operation::Get] {
        let input = if operation == wire::Operation::Get {
            serde_json::to_value(&target).unwrap()
        } else {
            serde_json::to_value(&put).unwrap()
        };
        assert!(matches!(
            raw(&running, origin.clone(), operation.spec(), &input).await,
            Err(ApiError::Unauthorized)
        ));
    }
    let scope = |credentials: Vec<CredentialRef>| Grant {
        principal: Principal::Device(device.record.id.clone()),
        scope: GrantScope::SshStdio {
            target: target.target.clone(),
            server: target.server.clone(),
            credentials,
        },
    };
    for reference in &refs {
        grant(&running, scope(vec![reference.clone()]), true).await;
    }
    assert!(
        matches!(
            raw(&running, origin.clone(), wire::Operation::Put.spec(), &put).await,
            Err(ApiError::Unauthorized)
        ),
        "two partial grants cannot be combined for credential export"
    );
    let complete = scope(refs.to_vec());
    grant(&running, complete.clone(), true).await;
    let state: wire::State = serde_json::from_value(
        raw(&running, origin.clone(), wire::Operation::Put.spec(), &put)
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(state.revision, "1");
    assert_eq!(state.config, Some(config.clone()));
    assert_eq!(state.apply_error, None);
    let request = wire::Change {
        target: target.clone(),
        expected: state.revision,
        config: None,
    };
    assert!(
        matches!(
            raw(
                &running,
                origin.clone(),
                wire::Operation::Refresh.spec(),
                &request
            )
            .await,
            Err(ApiError::Unauthorized)
        ),
        "stdio management must not imply Use"
    );
    assert!(
        matches!(
            raw(
                &running,
                origin.clone(),
                rsi_configuration_api::McpOperation::Refresh.spec(),
                &json!({"server":target.server})
            )
            .await,
            Err(ApiError::Unauthorized)
        ),
        "ordinary MCP refresh must not bypass exact target authority"
    );
    let mut malicious = put.clone();
    malicious.expected = "1".into();
    if let TransportConfig::SshStdio { environment, .. } =
        &mut malicious.config.as_mut().unwrap().transport
    {
        environment.insert(
            "EXTRA".into(),
            EnvironmentValue::Credential {
                reference: CredentialRef::new("rsi.mcp", "unrelated").unwrap(),
            },
        );
    }
    assert!(matches!(
        raw(
            &running,
            origin.clone(),
            wire::Operation::Put.spec(),
            &malicious
        )
        .await,
        Err(ApiError::Unauthorized)
    ));
    let state: wire::State = serde_json::from_value(
        raw(
            &running,
            origin.clone(),
            wire::Operation::Remove.spec(),
            &request,
        )
        .await
        .unwrap(),
    )
    .unwrap();
    assert_eq!(state.revision, "2");
    assert!(state.config.is_none());
    let rejected = raw(&running, origin.clone(), wire::Operation::Put.spec(), &put).await;
    assert!(
        matches!(rejected, Err(ApiError::Domain(_))),
        "deleted identity cannot be recreated by stale revision zero: {rejected:?}"
    );
    put.expected = "2".into();
    raw(&running, origin.clone(), wire::Operation::Put.spec(), &put)
        .await
        .unwrap();
    grant(&running, complete.clone(), false).await;
    assert!(matches!(
        raw(&running, origin, wire::Operation::Get.spec(), &target).await,
        Err(ApiError::Unauthorized)
    ));
    assert!(running.shutdown().await.is_clean());
    let running =
        RunningRsi::boot_host_profile(composition(fixture.paths.clone()), &host_profile(&fixture))
            .await
            .unwrap();
    let origin = CallOrigin::Device(
        running
            .device_authentication()
            .unwrap()
            .authenticate(&device.token)
            .unwrap(),
    );
    target.host_epoch = running.connection_description().unwrap().host_epoch.clone();
    grant(&running, complete, true).await;
    let state: wire::State = serde_json::from_value(
        raw(
            &running,
            origin.clone(),
            wire::Operation::Get.spec(),
            &target,
        )
        .await
        .unwrap(),
    )
    .unwrap();
    assert_eq!(state.revision, "3");
    assert_eq!(state.config, Some(config));
    let status: rsi_mcp::McpStatus = serde_json::from_value(
        raw(
            &running,
            origin,
            rsi_configuration_api::McpOperation::Status.spec(),
            &json!({}),
        )
        .await
        .unwrap(),
    )
    .unwrap();
    assert_eq!(status.servers.len(), 1);
    assert!(!status.servers[0].ready);
    assert_eq!(
        status.servers[0].epoch, "0",
        "cold restore must not resolve credentials or launch the configured target server"
    );
    assert!(running.shutdown().await.is_clean());
}
