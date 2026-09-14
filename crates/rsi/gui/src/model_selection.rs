use super::{Attachment, GuiApplication};
use crate::application::{Result, error};
use rsi_agent_session_protocol::{CommandArguments, DomainRequestId, ModelSelection};

impl Attachment {
    pub(super) fn selection(
        &self,
        projections: Option<&rsi_session_protocol::ProjectionSnapshot>,
    ) -> ModelSelection {
        projections
            .and_then(|snapshot| {
                snapshot
                    .snapshot()
                    .entries()
                    .iter()
                    .find(|entry| entry.producer().as_str() == "rsi.model-selection.view")
            })
            .and_then(|entry| entry.view())
            .and_then(|view| serde_json::from_value(view.value().clone()).ok())
            .unwrap_or_else(|| self.model.lock().expect("model selection poisoned").clone())
    }
}
impl GuiApplication {
    pub(super) async fn select_model(
        &self,
        pane: crate::SurfaceId,
        generation: &str,
        selection: ModelSelection,
    ) -> Result<()> {
        selection.validate().map_err(error)?;
        let attached = self.pane(pane)?.attachment(generation)?;
        let description = self
            .models
            .describe_model(&selection.model)
            .await
            .map_err(error)?;
        description
            .profile()
            .reasoning_efforts()
            .resolve(selection.reasoning_effort.as_ref())
            .map_err(error)?;
        // Recheck attachment before admitting a new mutation after the read.
        self.pane(pane)?.attachment(generation)?;
        let id = DomainRequestId::new(rsi_ui::fresh_identity("model-selection").map_err(error)?)
            .map_err(error)?;
        let arguments = CommandArguments::new(serde_json::to_value(&selection).map_err(error)?)
            .map_err(error)?;
        let result = attached
            .submission
            .model_command
            .execute(&attached.controller, "model-selection", arguments, id)
            .await;
        if result.is_ok() {
            *attached.model.lock().expect("model selection poisoned") = selection;
            *attached
                .model_description
                .lock()
                .expect("model description poisoned") = Some(description);
        }
        self.changed();
        result.map(|_| ()).map_err(error)
    }
    pub(super) async fn refresh_model(
        &self,
        pane: crate::SurfaceId,
        generation: &str,
    ) -> Result<()> {
        let attached = self.pane(pane)?.attachment(generation)?;
        if let Some(pending) = attached.submission.model_command.view().pending {
            let result = attached
                .submission
                .model_command
                .refresh(&attached.controller)
                .await;
            self.changed();
            result.map_err(error)?;
            let selection: ModelSelection =
                serde_json::from_value(pending.arguments.value().clone()).map_err(error)?;
            *attached.model.lock().expect("model selection poisoned") = selection;
        }
        let selection = {
            let state = attached.renderer.state.lock().expect("renderer poisoned");
            attached.selection(state.projections.as_ref())
        };
        let description = self.models.describe_model(&selection.model).await;
        self.pane(pane)?.attachment(generation)?;
        let state = attached.renderer.state.lock().expect("renderer poisoned");
        if attached.selection(state.projections.as_ref()).model == selection.model
            && let Ok(description) = description
        {
            *attached
                .model_description
                .lock()
                .expect("model description poisoned") = Some(description);
            self.changed();
        }
        Ok(())
    }
}
