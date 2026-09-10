use super::*;
use rsi_api_protocol::ApiError;
use rsi_workspace_protocol::*;
use std::{collections::VecDeque, path::Path, sync::Mutex, time::Duration};

#[derive(Debug)]
struct WorkspaceFailures {
    errors: Mutex<VecDeque<WorkspaceError>>,
    paths: Mutex<Vec<PathBuf>>,
}

#[async_trait::async_trait]
impl WorkspaceRegistry for WorkspaceFailures {
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
    async fn get_or_create(&self, path: &Path) -> rsi_workspace_protocol::Result<WorkspaceRecord> {
        self.paths.lock().unwrap().push(path.to_owned());
        Err(self
            .errors
            .lock()
            .unwrap()
            .pop_front()
            .expect("bounded attempts"))
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
        let registry = Arc::new(WorkspaceFailures {
            errors: Mutex::new(errors.into()),
            paths: Mutex::new(Vec::new()),
        });
        let (mut client, _, runtime, surface) = client().await;
        let cwd = PathBuf::from(client.state.header.canonical_cwd());
        let session = client.state.header.session_id().clone();
        client.workspace = registry.clone();
        client.state.editor.insert("preserved draft").unwrap();
        let started = tokio::time::Instant::now();
        client.action(Action::New);
        let Err(failure) = client.tasks.next().await.unwrap().result else {
            panic!("registration failed before Session creation")
        };
        assert!(failure.to_string().contains(&final_error), "{failure}");
        assert_eq!(
            registry.paths.lock().unwrap().as_slice(),
            vec![cwd; attempts]
        );
        assert_eq!(started.elapsed(), Duration::from_millis(elapsed));
        assert_eq!(client.state.header.session_id(), &session);
        assert_eq!(client.state.editor.text(), "preserved draft");
        surface.stop().await;
        assert!(runtime.shutdown().await.is_clean());
    }
}
