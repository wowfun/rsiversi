//! Finite Workflow reads and coalesced lifecycle invalidation over existing observation.
use crate::SessionController;
use rsi_session_protocol::{
    Result, WorkflowCursor, WorkflowDetail, WorkflowList, WorkflowPage, WorkflowRead,
    WorkflowReadiness,
};
/// One bounded pane selection; no lifecycle state is reduced here.
#[derive(Clone, Debug)]
#[allow(missing_docs)]
pub enum WorkflowSelection {
    Latest,
    History(WorkflowCursor),
    Detail(WorkflowRead),
    Child {
        run_id: rsi_agent_session_protocol::ProgramRunId,
        session_id: rsi_agent_session_protocol::SessionId,
    },
}
impl SessionController {
    /// Current finite workbench selection.
    ///
    /// # Panics
    /// Panics if the selection lock was poisoned by a previous panic.
    pub fn workflow_selection(&self) -> WorkflowSelection {
        self.workflow_selection
            .lock()
            .expect("workflow selection poisoned")
            .clone()
    }
    /// Replaces selection without executing a Workflow.
    ///
    /// # Panics
    /// Panics if the selection lock was poisoned by a previous panic.
    pub fn select_workflow_view(&self, selection: WorkflowSelection) {
        *self
            .workflow_selection
            .lock()
            .expect("workflow selection poisoned") = selection;
    }
    /// Coalesced hints carried by the existing Session observation.
    pub fn workflow_changes(&self) -> tokio::sync::watch::Receiver<u64> {
        self.workflow_changed.subscribe()
    }
    /// Coarse readiness, without executing Node or requiring configuration grants.
    pub async fn workflow_readiness(&self) -> Result<WorkflowReadiness> {
        self.handle.workflow_readiness().await
    }
    /// Reads current descending history; unpublished drafts have an empty history.
    pub async fn list_workflows(&self, cursor: Option<WorkflowCursor>) -> Result<WorkflowPage> {
        self.handle
            .list_workflows(WorkflowList { cursor, limit: 8 })
            .await
    }
    /// Reads the selected bounded canonical detail page.
    pub async fn read_workflow(&self, request: WorkflowRead) -> Result<WorkflowDetail> {
        self.handle.read_workflow(request).await
    }
    /// Accepts user cancellation; subsequent reads establish cleanup and terminal state.
    pub async fn cancel_workflow(
        &self,
        run: &rsi_agent_session_protocol::ProgramRunId,
    ) -> Result<rsi_agent_turn_protocol::ProgramCancelReceipt> {
        self.handle.cancel_workflow(run).await
    }
}
