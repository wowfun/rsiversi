use super::*;
use rsi_agent_session_protocol::{
    AgentPresetId, FrozenAgentSettings, SessionHeader, SessionId, TurnId,
};
use rsi_agent_turn_protocol::TurnClaimIssuer;
use rsi_api_protocol::{AuthenticatedDevice, DeviceId};
use rsi_execution::ExecutionLocation;
use rsi_execution::{ExecutionLease, ExecutionTargetId};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use tokio_util::sync::CancellationToken;

#[allow(dead_code)]
#[path = "../../../../fixtures/rsi/execution/metadata.rs"]
mod metadata;

#[derive(Debug, Default)]
struct Access {
    granted: AtomicBool,
}
impl ExecutionResolver for Access {
    fn visibility(
        &self,
        _: &rsi_api_protocol::CallOrigin,
    ) -> rsi_api_protocol::Result<rsi_execution::ExecutionVisibility> {
        panic!("unexpected enumeration in focused resolver fixture")
    }

    fn admit(&self, origin: &CallOrigin, _: &ExecutionLocation) -> Result<ExecutionOperation> {
        match origin {
            CallOrigin::Local => panic!("history borrowed Service Local authority"),
            CallOrigin::Device(device)
                if !device.revoked.is_cancelled() && self.granted.load(Ordering::SeqCst) =>
            {
                Ok(ExecutionOperation::new(()))
            }
            CallOrigin::Device(_) => Err(ApiError::Unauthorized),
        }
    }
    fn lease(&self, _: CallOrigin, _: &ExecutionLocation) -> Result<ExecutionLease> {
        panic!("history attempted an online execution connection")
    }
}
fn coordinates(path: &str) -> ExecutionCoordinates {
    ExecutionCoordinates::new(
        ExecutionLocation::Ssh {
            target: ExecutionTargetId::parse("a".repeat(32)).unwrap(),
        },
        path,
    )
    .unwrap()
}
#[test]
fn discovery_principals_use_canonical_device_identity() {
    use sha2::{Digest as _, Sha256};
    let device = DeviceId::from_bytes([0xab; 16]);
    let authority = |id| {
        HistoryAuthority::Caller(CallOrigin::Device(AuthenticatedDevice {
            id,
            revoked: CancellationToken::new(),
        }))
    };
    let principal = authority(device.clone()).principal();
    let identity = format!("device:{}", device.as_str());
    assert_eq!(principal, hex::encode(Sha256::digest(identity.as_bytes())));
    assert_eq!(principal, authority(device).principal());
    assert_ne!(
        principal,
        authority(DeviceId::from_bytes([0xac; 16])).principal()
    );
    assert_ne!(
        principal,
        HistoryAuthority::Caller(CallOrigin::Local).principal()
    );
}
#[test]
fn offline_history_rechecks_the_actual_principal_without_opening_a_connection() {
    let access = Access::default();
    let revoked = CancellationToken::new();
    let authority = HistoryAuthority::Caller(CallOrigin::Device(AuthenticatedDevice {
        id: DeviceId::from_bytes([1; 16]),
        revoked: revoked.clone(),
    }));
    let coords = coordinates("/remote-only");
    assert!(matches!(
        authority.admit(&access, &coords),
        Err(ApiError::Unauthorized)
    ));
    access.granted.store(true, Ordering::SeqCst);
    assert!(authority.admit(&access, &coords).is_ok());
    access.granted.store(false, Ordering::SeqCst);
    assert!(matches!(
        authority.admit(&access, &coords),
        Err(ApiError::Unauthorized)
    ));
    access.granted.store(true, Ordering::SeqCst);
    revoked.cancel();
    assert!(matches!(
        authority.admit(&access, &coords),
        Err(ApiError::Unauthorized)
    ));
}
#[test]
fn agent_history_uses_its_original_lease_and_rejects_other_workspaces() {
    let coords = coordinates("/remote-only");
    let header = Arc::new(
        SessionHeader::new(
            SessionId::new("history-caller").unwrap(),
            1,
            coords.clone(),
            AgentPresetId::new("fixture").unwrap(),
            FrozenAgentSettings::new(
                "fixture",
                "system",
                rsi_ai_protocol::ModelRef::new("fixture", "model").unwrap(),
                rsi_sandbox::SandboxMode::ReadOnly,
                false,
            )
            .unwrap(),
        )
        .unwrap(),
    );
    let issuer = TurnClaimIssuer::new();
    let claim = issuer.issue(
        "fixture".into(),
        1,
        header.session_id().clone(),
        TurnId::new("turn").unwrap(),
        header,
        1,
        1,
        1,
    );
    let gate = Arc::new(metadata::Gate::default());
    let missing = HistoryAuthority::Agent(Arc::new(issuer.agent_caller(&claim).unwrap()));
    assert!(matches!(
        missing.admit(&Access::default(), &coords),
        Err(ApiError::Unauthorized)
    ));
    let claim = issuer
        .bind_execution(
            claim,
            Some(metadata::lease(coords.location().clone(), gate.clone(), 1)),
        )
        .unwrap();
    let authority = HistoryAuthority::Agent(Arc::new(issuer.agent_caller(&claim).unwrap()));
    let workspace = rsi_workspace_protocol::WorkspaceId::from_coordinates(&coords);
    assert!(
        authority
            .narrow(&rsi_history_api::QueryScope::Workspace {
                workspace: workspace.clone()
            })
            .is_ok()
    );
    assert!(matches!(
        authority.narrow(&rsi_history_api::QueryScope::AccessibleHost),
        Err(ApiError::Unauthorized)
    ));
    assert!(matches!(
        authority.narrow(&rsi_history_api::QueryScope::Workspace {
            workspace: rsi_workspace_protocol::WorkspaceId::from_coordinates(&coordinates(
                "/another-workspace"
            )),
        }),
        Err(ApiError::Unauthorized)
    ));
    for (requested, allowed) in [
        (workspace, true),
        (
            rsi_workspace_protocol::WorkspaceId::from_coordinates(&coordinates(
                "/another-workspace",
            )),
            false,
        ),
    ] {
        let scope = rsi_history_api::QueryScope::Conversation {
            source: rsi_history_api::Scope {
                workspace: requested,
                conversation: rsi_history_api::ConversationIdentity::Native(
                    SessionId::new("another-source").unwrap(),
                ),
            },
        };
        assert_eq!(authority.narrow(&scope).is_ok(), allowed);
    }
    assert!(authority.admit(&Access::default(), &coords).is_ok());
    assert!(matches!(
        authority.admit(&Access::default(), &coordinates("/another-workspace")),
        Err(ApiError::Unauthorized)
    ));
    gate.revoked.store(true, Ordering::SeqCst);
    assert!(matches!(
        authority.admit(&Access::default(), &coords),
        Err(ApiError::Unauthorized)
    ));
}

#[test]
fn a_prepared_source_reply_is_not_published_after_use_withdrawal() {
    let coords = coordinates("/remote-only");
    let access = Access {
        granted: AtomicBool::new(true),
    };
    let authority = HistoryAuthority::Caller(CallOrigin::Device(AuthenticatedDevice {
        id: DeviceId::from_bytes([4; 16]),
        revoked: CancellationToken::new(),
    }));
    let admission = authority.admit(&access, &coords).unwrap();
    let protection = CancellationToken::new();
    let scope = rsi_history_api::Scope {
        workspace: rsi_workspace_protocol::WorkspaceId::from_coordinates(&coords),
        conversation: rsi_history_api::ConversationIdentity::External(
            rsi_acp_protocol::observation::ConversationId::new("saved-source").unwrap(),
        ),
    };
    let request = rsi_history_api::Request::Rebuild { scope };
    let coverage = rsi_history_api::Coverage {
        source: rsi_agent_session_protocol::ReferenceSource::Observed {
            owner: "acp".into(),
            id: "saved-source".into(),
            epoch: 1,
        },
        indexed_through: "0".into(),
        observed_through: "0".into(),
        omissions: "0".into(),
        has_more: false,
    };
    let reply = rsi_history_api::Reply::Coverage { coverage };
    assert!(
        crate::publish_source_reply(
            &authority,
            &access,
            &coords,
            &protection,
            &request,
            reply.clone()
        )
        .is_ok()
    );
    access.granted.store(false, Ordering::SeqCst);
    assert!(matches!(
        crate::publish_source_reply(
            &authority,
            &access,
            &coords,
            &protection,
            &request,
            reply.clone()
        ),
        Err(ApiError::Unauthorized)
    ));
    access.granted.store(true, Ordering::SeqCst);
    protection.cancel();
    assert!(matches!(
        crate::publish_source_reply(&authority, &access, &coords, &protection, &request, reply),
        Err(ApiError::Unauthorized)
    ));
    drop(admission);
}
