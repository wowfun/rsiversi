use super::*;
use rsi_api_protocol::ApiError;
use rsi_workspace_protocol::*;
use std::{collections::VecDeque, path::Path, sync::Mutex, time::Duration};

#[derive(Debug)]
struct WorkspaceFixture {
    errors: Mutex<VecDeque<WorkspaceError>>,
    registrations: Mutex<Vec<ExecutionCoordinates>>,
}

#[async_trait::async_trait]
impl WorkspaceRegistry for WorkspaceFixture {
    async fn find(
        &self,
        _: &rsi_workspace_protocol::WorkspaceKey,
    ) -> rsi_workspace_protocol::Result<Option<rsi_workspace_protocol::WorkspaceRecord>> {
        panic!("coordinate lookup is not used by this fixture")
    }

    async fn display_order(
        &self,
    ) -> rsi_workspace_protocol::Result<rsi_workspace_protocol::WorkspaceDisplayOrder> {
        unreachable!("display ordering is not used by this fixture")
    }
    async fn insert_before(
        &self,
        _: &rsi_workspace_protocol::WorkspaceId,
        _: Option<&rsi_workspace_protocol::WorkspaceId>,
    ) -> rsi_workspace_protocol::Result<rsi_workspace_protocol::WorkspaceDisplayOrder> {
        unreachable!("display ordering is not used by this fixture")
    }

    async fn rename(
        &self,
        _id: &rsi_workspace_protocol::WorkspaceId,
        _name: rsi_workspace_protocol::WorkspaceName,
    ) -> rsi_workspace_protocol::Result<rsi_workspace_protocol::WorkspaceRecord> {
        unreachable!("rename is not used by this fixture")
    }
    async fn order_seed(
        &self,
    ) -> rsi_workspace_protocol::Result<rsi_workspace_protocol::WorkspaceOrderSeed> {
        unreachable!("workspace order membership is not used by this fixture")
    }
    async fn get(&self, _: &WorkspaceId) -> rsi_workspace_protocol::Result<WorkspaceRecord> {
        unreachable!("New registers its canonical directory")
    }
    async fn list(
        &self,
        _: Option<WorkspaceCursor>,
        _: usize,
    ) -> rsi_workspace_protocol::Result<WorkspacePage> {
        unreachable!("New does not list workspaces")
    }
    async fn register_at(
        &self,
        location: &ExecutionLocation,
        path: &Path,
    ) -> rsi_workspace_protocol::Result<WorkspaceRecord> {
        let coordinates =
            ExecutionCoordinates::new(location.clone(), path.to_str().unwrap()).unwrap();
        self.registrations.lock().unwrap().push(coordinates.clone());
        if let Some(error) = self.errors.lock().unwrap().pop_front() {
            return Err(error);
        }
        Ok(WorkspaceRecord::new(
            WorkspaceId::from_bytes([1; 32]),
            coordinates,
        ))
    }
    async fn status(&self, _: &WorkspaceId) -> rsi_workspace_protocol::Result<WorkspaceStatus> {
        unreachable!("New does not query directory status")
    }
    async fn delete_registration(&self, _: &WorkspaceId) -> rsi_workspace_protocol::Result<bool> {
        unreachable!("New does not delete registrations")
    }
}

#[tokio::test(start_paused = true)]
async fn new_retries_only_api_capacity_with_a_frozen_directory_and_finite_backoff() {
    let capacity = WorkspaceError::Api(ApiError::Capacity);
    let invalid = WorkspaceError::InvalidInput("directory disappeared".into());
    for (errors, elapsed) in [
        (vec![capacity.clone(), capacity.clone(), invalid], 150),
        (vec![capacity; 5], 750),
        (vec![WorkspaceError::Api(ApiError::OutcomeUnknown)], 0),
        (vec![WorkspaceError::Capacity], 0),
    ] {
        let attempts = errors.len();
        let final_error = errors.last().unwrap().to_string();
        let registry = Arc::new(WorkspaceFixture {
            errors: Mutex::new(errors.into()),
            registrations: Mutex::default(),
        });
        let (mut client, _, runtime, surface) = client().await;
        let coordinates = client.state.header.coordinates().clone();
        let session = client.state.header.session_id().clone();
        client.workspace = registry.clone();
        client.state.editor.insert("preserved draft").unwrap();
        let started = tokio::time::Instant::now();
        client.action(Action::New(None));
        let Err(failure) = client.tasks.next().await.unwrap().result else {
            panic!("registration failed before Session creation")
        };
        assert!(failure.to_string().contains(&final_error), "{failure}");
        assert_eq!(
            registry.registrations.lock().unwrap().as_slice(),
            vec![coordinates; attempts]
        );
        assert_eq!(started.elapsed(), Duration::from_millis(elapsed));
        assert_eq!(client.state.header.session_id(), &session);
        assert_eq!(client.state.editor.text(), "preserved draft");
        surface.stop().await;
        assert!(runtime.shutdown().await.is_clean());
    }
}

#[tokio::test]
async fn new_with_explicit_preset_stays_on_the_attached_ssh_target() {
    let (mut client, handle, runtime, surface) = client().await;
    let registry = Arc::new(WorkspaceFixture {
        errors: Mutex::default(),
        registrations: Mutex::default(),
    });
    let coordinates = ExecutionCoordinates::new(
        ExecutionLocation::Ssh {
            target: rsi_agent_session_protocol::ExecutionTargetId::parse("a".repeat(32)).unwrap(),
        },
        "/srv/project",
    )
    .unwrap();
    let header = &client.state.header;
    client.state.header = SessionHeader::new(
        header.session_id().clone(),
        1,
        coordinates.clone(),
        AgentPresetId::new("standard").unwrap(),
        header.settings().clone(),
    )
    .unwrap();
    *handle.draft_header.lock().unwrap() = Some(client.state.header.clone());
    client.workspace = registry.clone();
    for (command, expected) in [("/new", None), ("/new standard", Some("standard"))] {
        client.state.editor.insert(command).unwrap();
        client.submit(MessageDelivery::NextTurn, false);
        let Some(setup::Command::New(preset)) = client.setup_command.take() else {
            panic!("expected a new session command");
        };
        client.action(Action::New(preset));
        let Update::Attached(attached) = client.tasks.next().await.unwrap().result.unwrap() else {
            panic!("expected the registered workspace conversation");
        };
        assert_eq!(attached.header, client.state.header);
        assert!(attached.inspection.is_none());
        let request = handle.created.lock().unwrap().pop().unwrap();
        assert_ne!(&request.session_id, attached.header.session_id());
        assert_eq!(request.workspace_id, WorkspaceId::from_bytes([1; 32]));
        assert_eq!(
            request.agent_preset_id.as_ref().map(AgentPresetId::as_str),
            expected
        );
        assert_eq!(
            registry.registrations.lock().unwrap().pop().unwrap(),
            coordinates
        );
    }
    client.state.editor.insert("/new invalid/target").unwrap();
    client.submit(MessageDelivery::NextTurn, false);
    assert!(client.setup_command.is_none());
    assert_eq!(client.state.editor.text(), "/new invalid/target");
    surface.stop().await;
    assert!(runtime.shutdown().await.is_clean());
}
