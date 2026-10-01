use super::super::{RunningRsi, composition, fixture, host_profile};
use super::{grant, raw};
use base64::Engine as _;
use rsi_api_protocol::{ApiError, CallOrigin};
use rsi_configuration_api::{
    leaf::{Grant, GrantScope, Principal},
    ssh::*,
};
use serde::{Serialize, de::DeserializeOwned};
use serde_json::json;
use std::os::unix::fs::PermissionsExt;
async fn ssh<T: DeserializeOwned>(
    running: &RunningRsi,
    origin: CallOrigin,
    op: Operation,
    input: &impl Serialize,
) -> Reply<T> {
    match raw(running, origin, op.spec(), input).await {
        Ok(value) => Ok(Ok(serde_json::from_value(value).unwrap())),
        Err(ApiError::Domain(bytes)) => Ok(Err(serde_json::from_slice(bytes.as_bytes()).unwrap())),
        Err(error) => Err(error),
    }
}
async fn catalog(running: &RunningRsi, origin: CallOrigin) -> Catalog {
    ssh(running, origin, Operation::Catalog, &json!({}))
        .await
        .unwrap()
        .unwrap()
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[expect(
    clippy::too_many_lines,
    reason = "One public lifecycle binds independent grants, Local trust, identity replacement and cold restore"
)]
async fn ssh_target_candidates_require_separate_use_manage_and_local_trust_across_restart() {
    let fixture = fixture("http://127.0.0.1:1");
    let running =
        RunningRsi::boot_host_profile(composition(fixture.paths.clone()), &host_profile(&fixture))
            .await
            .unwrap();
    let device = running
        .device_administration()
        .unwrap()
        .register("ssh-device")
        .await
        .unwrap();
    let origin = CallOrigin::Device(
        running
            .device_authentication()
            .unwrap()
            .authenticate(&device.token)
            .unwrap(),
    );
    let other = running
        .device_administration()
        .unwrap()
        .register("other-device")
        .await
        .unwrap();
    let other_origin = CallOrigin::Device(
        running
            .device_authentication()
            .unwrap()
            .authenticate(&other.token)
            .unwrap(),
    );
    let page = catalog(&running, origin.clone()).await;
    assert!(page.targets.is_empty());
    let mut put = PutCandidate {
        host_epoch: page.host_epoch.clone(),
        expected: "0".into(),
        candidate: Candidate {
            target: rsi_execution_protocol::ExecutionTargetId::parse("a".repeat(32)).unwrap(),
            name: "Target A".into(),
            endpoint: rsi_ssh_protocol::SshEndpoint::new("127.0.0.1", 1, "fixture").unwrap(),
        },
    };
    assert!(matches!(
        ssh::<Target>(&running, origin.clone(), Operation::PutCandidate, &put).await,
        Err(ApiError::Unauthorized)
    ));
    raw(
        &running,
        CallOrigin::Local,
        rsi_configuration_api::ConfigurationOperation::SetGrant.spec(),
        &json!({"device":device.record.id,"expected_revision":"0","granted":true}),
    )
    .await
    .unwrap();
    let target: Target = ssh(&running, origin.clone(), Operation::PutCandidate, &put)
        .await
        .unwrap()
        .unwrap();
    assert!(!target.permissions.use_target && !target.permissions.manage);
    assert!(target.fingerprint.is_none() && target.connection_epoch.is_none());
    assert!(matches!(
        ssh::<Target>(&running, origin.clone(), Operation::PutCandidate, &put).await,
        Ok(Err(Failure::Conflict {}))
    ));
    assert!(catalog(&running, other_origin).await.targets.is_empty());
    let selection = Selection {
        host_epoch: page.host_epoch.clone(),
        target: target.candidate.target.clone(),
        revision: target.revision.clone(),
    };
    let mut connection = ConnectionRequest {
        selection: selection.clone(),
        expected_connection_epoch: None,
    };
    assert!(matches!(
        ssh::<Target>(&running, origin.clone(), Operation::Connect, &connection).await,
        Err(ApiError::Unauthorized)
    ));
    put.expected = "1".into();
    put.candidate.name = "Changed".into();
    assert!(matches!(
        ssh::<Target>(&running, origin.clone(), Operation::PutCandidate, &put).await,
        Err(ApiError::Unauthorized)
    ));
    grant(
        &running,
        Grant {
            principal: Principal::Device(device.record.id.clone()),
            scope: GrantScope::SshUse {
                target: target.candidate.target.clone(),
            },
        },
        true,
    )
    .await;
    assert!(matches!(
        ssh::<Target>(&running, origin.clone(), Operation::Connect, &connection).await,
        Ok(Err(Failure::TrustRequired {}))
    ));

    let field = |value: &[u8]| {
        [
            u32::try_from(value.len()).unwrap().to_be_bytes().as_slice(),
            value,
        ]
        .concat()
    };
    let key = rsi_ssh_protocol::SshHostKey::new(
        "ssh-ed25519",
        base64::engine::general_purpose::STANDARD
            .encode([field(b"ssh-ed25519"), field(&[7; 32])].concat()),
    )
    .unwrap();
    let identity_root = tempfile::tempdir().unwrap();
    let identity = identity_root.path().join("identity");
    std::fs::write(&identity, b"private-fixture-identity-never-returned").unwrap();
    std::fs::set_permissions(&identity, std::fs::Permissions::from_mode(0o600)).unwrap();
    let trust = ConfirmTrust {
        selection,
        fingerprint: key.fingerprint(),
        host_key: key,
        identity_path: identity.to_str().unwrap().into(),
    };
    assert!(matches!(
        ssh::<Target>(&running, origin.clone(), Operation::ConfirmTrust, &trust).await,
        Err(ApiError::Unauthorized)
    ));
    let trusted: Target = ssh(&running, CallOrigin::Local, Operation::ConfirmTrust, &trust)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(trusted.revision, "2");
    connection.selection.revision = trusted.revision.clone();
    // This test executable has no distribution receipt, so it cannot deploy an ad hoc helper.
    assert!(matches!(
        ssh::<Target>(&running, origin.clone(), Operation::Connect, &connection).await,
        Ok(Err(Failure::HelperUnavailable {}))
    ));
    std::fs::write(&identity, b"replaced-fixture-identity").unwrap();
    assert!(matches!(
        ssh::<Target>(&running, origin.clone(), Operation::Connect, &connection).await,
        Ok(Err(Failure::IdentityUnavailable {}))
    ));
    let redacted = serde_json::to_string(&catalog(&running, origin.clone()).await).unwrap();
    assert!(!redacted.contains("identity") && !redacted.contains(identity.to_str().unwrap()));
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
    let restored = catalog(&running, origin.clone()).await;
    assert_eq!(restored.targets.len(), 1);
    assert_eq!(restored.targets[0].fingerprint, trusted.fingerprint);
    assert!(!restored.targets[0].connected && restored.targets[0].connection_epoch.is_none());
    assert!(matches!(
        ssh::<Target>(&running, origin.clone(), Operation::Connect, &connection).await,
        Err(ApiError::Invalid(_))
    ));
    connection.selection.host_epoch = restored.host_epoch.clone();
    assert!(matches!(
        ssh::<Target>(&running, origin.clone(), Operation::Connect, &connection).await,
        Ok(Err(Failure::IdentityUnavailable {}))
    ));
    grant(
        &running,
        Grant {
            principal: Principal::Device(device.record.id.clone()),
            scope: GrantScope::SshUse {
                target: target.candidate.target.clone(),
            },
        },
        false,
    )
    .await;
    assert!(matches!(
        ssh::<Target>(&running, origin.clone(), Operation::Connect, &connection).await,
        Err(ApiError::Unauthorized)
    ));
    grant(
        &running,
        Grant {
            principal: Principal::Device(device.record.id),
            scope: GrantScope::SshManage {
                target: target.candidate.target.clone(),
            },
        },
        true,
    )
    .await;
    put.host_epoch = restored.host_epoch;
    put.expected = "2".into();
    let edited: Target = ssh(&running, origin.clone(), Operation::PutCandidate, &put)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(edited.revision, "3");
    assert!(
        edited.fingerprint.is_none() && !edited.permissions.use_target && edited.permissions.manage
    );
    assert!(running.shutdown().await.is_clean());
}
