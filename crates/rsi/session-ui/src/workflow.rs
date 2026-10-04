use super::{
    ActionInput, ActionTarget, BoxFuture, Context, Deserialize, FieldWindow, Result, Serialize,
    SessionController, SurfaceRenderer, UiAction, UiElement, UiError, UiView, controller,
};
use rsi_agent_session_protocol::{ProgramOutcome, ProgramRunId};
use rsi_client::WorkflowSelection;
use rsi_session_protocol::{WorkflowCursor, WorkflowDetail, WorkflowRead};
#[derive(Debug)]
pub(super) struct Card;
impl SurfaceRenderer for Card {
    fn model(&self, target: Context) -> BoxFuture<'_, Result<rsi_ui::UiModel>> {
        Box::pin(async move {
            rsi_ui::UiModel::standard(read(&target, controller(&target)?.as_ref(), None).await?)
                .map_err(Into::into)
        })
    }
}
#[derive(Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum Input {
    Latest,
    Refresh,
    History { cursor: WorkflowCursor },
    Detail { request: WorkflowRead },
    Cancel { run: ProgramRunId },
    Child { request: WorkflowRead, ordinal: u32 },
}
fn button(label: impl Into<String>, value: Input) -> UiElement {
    UiElement::Button {
        action: "workflow".into(),
        label: label.into(),
        value: serde_json::to_value(value).expect("closed workflow action"),
    }
}
fn field(label: impl Into<String>, value: impl Into<String>) -> UiElement {
    UiElement::Field {
        label: label.into(),
        value: value.into(),
    }
}
fn status(run: &rsi_agent_turn_protocol::ProgramOverview) -> String {
    if let Some(outcome) = &run.outcome {
        return match outcome {
            ProgramOutcome::Completed => "Completed",
            ProgramOutcome::Cancelled => "Cancelled",
            ProgramOutcome::Interrupted => "Interrupted",
            ProgramOutcome::Failed { .. } => "Failed",
        }
        .into();
    }
    if run.orphaned {
        "Orphaned · restart required"
    } else if run.cancelling {
        "Cancelling · cleanup pending"
    } else if run.detached {
        "Background running"
    } else if run.started {
        "Running"
    } else {
        "Accepted"
    }
    .into()
}
fn detail_request(run: ProgramRunId) -> WorkflowRead {
    WorkflowRead {
        run_id: run,
        expected_control_seq: None,
        children_offset: 0,
        result_offset: None,
        script_offset: None,
    }
}
fn child_request(run: ProgramRunId, control_seq: u64, ordinal: u32) -> WorkflowRead {
    WorkflowRead {
        run_id: run,
        expected_control_seq: Some(control_seq),
        children_offset: ((ordinal - 1) as usize / rsi_session_protocol::WORKFLOW_CHILD_PAGE_SIZE)
            * rsi_session_protocol::WORKFLOW_CHILD_PAGE_SIZE,
        result_offset: None,
        script_offset: None,
    }
}
fn diagnostic(error: impl std::fmt::Display) -> String {
    FieldWindow::text(&error.to_string(), 0, 4096)
        .expect("diagnostic bound")
        .text
}
fn readiness_text(ready: &rsi_session_protocol::WorkflowReadiness) -> String {
    format!(
        "Runtime: {} · execution: {} · tools: {} · plan: {}",
        match ready.runtime.kind {
            rsi_session_protocol::WorkflowRuntimeKind::Absent => "Not configured",
            rsi_session_protocol::WorkflowRuntimeKind::Disabled =>
                "Disabled in desired Host configuration",
            rsi_session_protocol::WorkflowRuntimeKind::Pending => "Applying Host configuration",
            rsi_session_protocol::WorkflowRuntimeKind::Available => "Available",
            rsi_session_protocol::WorkflowRuntimeKind::Unavailable => "Unavailable",
        },
        if ready.runtime.available {
            "active"
        } else {
            "unavailable"
        },
        match ready.tools {
            rsi_session_protocol::WorkflowTools::Enabled => "Enabled",
            rsi_session_protocol::WorkflowTools::Disabled => "Disabled for this Session",
            rsi_session_protocol::WorkflowTools::NotResident => "Not resident (cold Session)",
            rsi_session_protocol::WorkflowTools::Unavailable => "Unavailable",
        },
        match ready.plan {
            rsi_session_protocol::WorkflowPlan::On => "On",
            rsi_session_protocol::WorkflowPlan::Off => "Off",
            rsi_session_protocol::WorkflowPlan::Unavailable => "Unavailable",
        }
    )
}
async fn read(
    target: &Context,
    controller: &SessionController,
    receipt: Option<String>,
) -> Result<UiView> {
    let selection = controller.workflow_selection();
    let mut elements = vec![
        button("Latest workflows", Input::Latest),
        button("Refresh current view", Input::Refresh),
    ];
    if let Some(receipt) = receipt {
        elements.push(UiElement::Text { text: receipt });
    }
    if !matches!(&selection, WorkflowSelection::Child { .. }) {
        match controller.workflow_readiness().await {
            Ok(ready) => {
                elements.push(field("Readiness", readiness_text(&ready)));
                if ready.runtime.host_restart_required {
                    elements.push(UiElement::Text { text: "Host configuration requires an explicit restart. Running workflows will be interrupted.".into() });
                }
                elements.push(UiElement::Text { text: if ready.local_execution { "Run a workflow in this Session to verify Node execution. Saving configuration does not execute Node." } else { "The selected target needs its own Node execution verification." }.into() });
            }
            Err(error) => elements.push(field("Readiness unavailable", diagnostic(error))),
        }
    }
    match selection {
        selected @ (WorkflowSelection::Latest | WorkflowSelection::History(_)) => {
            let cursor = match selected {
                WorkflowSelection::History(cursor) => Some(cursor),
                _ => None,
            };
            match controller.list_workflows(cursor).await {
                Ok(page) => {
                    if page.runs.is_empty() {
                        elements.push(UiElement::Text { text: "No accepted workflows on this page. Select the workflow preset after configuring your Host Node runtime, then ask the agent to use a local Workflow Skill.".into() });
                    }
                    if page.runs.iter().any(|run| run.outcome.is_none()) {
                        elements.push(UiElement::Text { text: "This Session's Workflow slot is occupied. Cancel its unfinished run to release it. A Host admits at most 8 live workflows; background runs have no total time limit.".into() });
                    }
                    for run in page.runs {
                        elements.push(field(
                            format!("Workflow {}", run.run_id),
                            format!(
                                "{} · accepted {} · control {} · children {}/{}",
                                status(&run),
                                run.accepted_control_seq,
                                run.control_seq,
                                run.settled_children,
                                run.children
                            ),
                        ));
                        if let Some(phase) = &run.phase {
                            elements.push(field("Phase", phase));
                        }
                        elements.push(button(
                            "Open workflow",
                            Input::Detail {
                                request: detail_request(run.run_id),
                            },
                        ));
                    }
                    if let Some(cursor) = page.next {
                        elements.push(button("Older workflows", Input::History { cursor }));
                    }
                }
                Err(error) => elements.push(field("History unavailable", diagnostic(error))),
            }
        }
        WorkflowSelection::Child { session_id, .. } => {
            let mut view = rsi_session_tree_ui::inspect_history(target, session_id).await?;
            view.elements
                .insert(0, button("Latest workflows", Input::Latest));
            view.elements
                .insert(1, button("Refresh current view", Input::Refresh));
            return Ok(view);
        }
        WorkflowSelection::Detail(request) => {
            match controller.read_workflow(request.clone()).await {
                Ok(detail) => detail_elements(&mut elements, detail),
                Err(error) => elements.push(field("Detail unavailable", diagnostic(error))),
            }
        }
    }
    elements.push(UiElement::Text { text: "Stop cancels foreground observation and its workflow. After detachment, Stop affects the conversation. Plan mode revokes running workflows.".into() });
    Ok(UiView {
        title: "Workflows".into(),
        elements,
    })
}
fn detail_elements(elements: &mut Vec<UiElement>, detail: WorkflowDetail) {
    let run = &detail.run;
    elements.push(field("Run", run.run_id.to_string()));
    elements.push(field("State", status(run)));
    if let Some(phase) = &run.phase {
        elements.push(field("Phase", phase));
    }
    if let Some(progress) = &run.progress {
        elements.push(field("Progress", progress));
    }
    if run.outcome.is_none() && !run.orphaned {
        elements.push(button(
            "Cancel workflow",
            Input::Cancel {
                run: run.run_id.clone(),
            },
        ));
    }
    if run.orphaned {
        elements.push(UiElement::Text { text: "The live owner is unavailable. Startup recovery will interrupt this run; cleanup cannot be proved from history alone.".into() });
    }
    for child in detail.children {
        elements.push(field(
            format!("Child {}", child.ordinal),
            format!(
                "{} · {}",
                child.session_id,
                child
                    .receipt
                    .as_ref()
                    .map_or("Running".to_owned(), |r| diagnostic(format!(
                        "{:?}",
                        r.outcome
                    )))
            ),
        ));
        elements.push(button(
            format!("Inspect child {}", child.ordinal),
            Input::Child {
                request: child_request(run.run_id.clone(), run.control_seq, child.ordinal),
                ordinal: child.ordinal,
            },
        ));
    }
    if let Some(offset) = detail.next_children_offset {
        elements.push(button(
            "More children",
            Input::Detail {
                request: WorkflowRead {
                    run_id: run.run_id.clone(),
                    expected_control_seq: Some(run.control_seq),
                    children_offset: offset,
                    result_offset: None,
                    script_offset: None,
                },
            },
        ));
    }
    fragment_elements(elements, run, detail.script, detail.result);
}
fn fragment_elements(
    elements: &mut Vec<UiElement>,
    run: &rsi_agent_turn_protocol::ProgramOverview,
    script: Option<rsi_session_protocol::WorkflowResultFragment>,
    result: Option<rsi_session_protocol::WorkflowResultFragment>,
) {
    if let Some(script) = script {
        elements.push(field("Frozen script", script.fragment));
        if let Some(offset) = script.next_offset {
            elements.push(button(
                "Next script page",
                Input::Detail {
                    request: WorkflowRead {
                        run_id: run.run_id.clone(),
                        expected_control_seq: Some(run.control_seq),
                        children_offset: 0,
                        result_offset: None,
                        script_offset: Some(offset),
                    },
                },
            ));
        }
    } else {
        elements.push(button(
            "Read frozen script",
            Input::Detail {
                request: WorkflowRead {
                    run_id: run.run_id.clone(),
                    expected_control_seq: Some(run.control_seq),
                    children_offset: 0,
                    result_offset: None,
                    script_offset: Some(0),
                },
            },
        ));
    }
    if let Some(result) = result {
        elements.push(field("Result", result.fragment));
        if let Some(offset) = result.next_offset {
            elements.push(button(
                "Next result page",
                Input::Detail {
                    request: WorkflowRead {
                        run_id: run.run_id.clone(),
                        expected_control_seq: Some(run.control_seq),
                        children_offset: 0,
                        result_offset: Some(offset),
                        script_offset: None,
                    },
                },
            ));
        }
    } else if run.result_ref.is_some() {
        elements.push(button(
            "Read result",
            Input::Detail {
                request: WorkflowRead {
                    run_id: run.run_id.clone(),
                    expected_control_seq: Some(run.control_seq),
                    children_offset: 0,
                    result_offset: Some(0),
                    script_offset: None,
                },
            },
        ));
    }
}
#[derive(Debug)]
pub(super) struct Action;
impl UiAction for Action {
    fn invoke(
        &self,
        target: ActionTarget,
        input: ActionInput,
    ) -> BoxFuture<'static, Result<UiView>> {
        Box::pin(async move {
            if !input.fields.is_empty() {
                return Err(UiError::Invalid(
                    "workflow actions have no editable fields".into(),
                ));
            }
            let input: Input =
                serde_json::from_value(input.value).map_err(|e| UiError::Invalid(e.to_string()))?;
            let controller = controller(target.context())?;
            let mut receipt = None;
            match input {
                Input::Latest => controller.select_workflow_view(WorkflowSelection::Latest),
                Input::Refresh => {
                    if let WorkflowSelection::Detail(mut request) = controller.workflow_selection()
                    {
                        let current = controller
                            .read_workflow(detail_request(request.run_id.clone()))
                            .await
                            .map_err(|error| UiError::Action(diagnostic(error)))?;
                        request.expected_control_seq = Some(current.run.control_seq);
                        controller.select_workflow_view(WorkflowSelection::Detail(request));
                    }
                }
                Input::History { cursor } => {
                    controller.select_workflow_view(WorkflowSelection::History(cursor));
                }
                Input::Detail { request } => {
                    request
                        .validate()
                        .map_err(|e| UiError::Invalid(e.to_string()))?;
                    controller.select_workflow_view(WorkflowSelection::Detail(request));
                }
                Input::Cancel { run } => {
                    receipt = Some(match controller.cancel_workflow(&run).await {
                        Ok(rsi_agent_turn_protocol::ProgramCancelReceipt::Accepted { control_seq, .. }) => format!("Cancellation accepted at control {control_seq}."),
                        Ok(rsi_agent_turn_protocol::ProgramCancelReceipt::AlreadyTerminal { outcome, .. }) => format!("Workflow already terminal: {outcome:?}"),
                        Ok(rsi_agent_turn_protocol::ProgramCancelReceipt::OrphanedRequiresRestart { .. }) => "Orphaned workflow requires Host restart.".into(),
                        Err(error) => diagnostic(error),
                    });
                    controller.select_workflow_view(WorkflowSelection::Detail(detail_request(run)));
                }
                Input::Child { request, ordinal } => {
                    request
                        .validate()
                        .map_err(|e| UiError::Invalid(e.to_string()))?;
                    if ordinal == 0
                        || ordinal > rsi_agent_session_protocol::MAXIMUM_PROGRAM_CHILDREN
                        || request.expected_control_seq.is_none()
                        || request.children_offset
                            != ((ordinal - 1) as usize
                                / rsi_session_protocol::WORKFLOW_CHILD_PAGE_SIZE)
                                * rsi_session_protocol::WORKFLOW_CHILD_PAGE_SIZE
                        || request.result_offset.is_some()
                        || request.script_offset.is_some()
                    {
                        return Err(UiError::Invalid("invalid child ordinal".into()));
                    }
                    let detail = controller
                        .read_workflow(request)
                        .await
                        .map_err(|e| UiError::Action(diagnostic(e)))?;
                    let child = detail
                        .children
                        .iter()
                        .find(|c| c.ordinal == ordinal)
                        .ok_or_else(|| {
                            UiError::Invalid("child is not admitted to this workflow".into())
                        })?;
                    controller.select_workflow_view(WorkflowSelection::Child {
                        run_id: detail.run.run_id,
                        session_id: child.session_id.clone(),
                    });
                }
            }
            read(target.context(), &controller, receipt).await
        })
    }
}

pub(super) fn run_button(run: ProgramRunId) -> UiElement {
    button(
        "Open workflow",
        Input::Detail {
            request: detail_request(run),
        },
    )
}

#[derive(Debug)]
pub(super) struct Link(pub ProgramRunId);
impl SurfaceRenderer for Link {
    fn model(&self, target: Context) -> BoxFuture<'_, Result<rsi_ui::UiModel>> {
        Box::pin(async move {
            let controller = controller(&target)?;
            let selected = match controller.workflow_selection() {
                WorkflowSelection::Detail(request) => request.run_id == self.0,
                WorkflowSelection::Child { run_id, .. } => run_id == self.0,
                _ => false,
            };
            if selected {
                return rsi_ui::UiModel::standard(read(&target, &controller, None).await?)
                    .map_err(Into::into);
            }
            rsi_ui::UiModel::standard(UiView {
                title: "Detached workflow".into(),
                elements: vec![run_button(self.0.clone())],
            })
            .map_err(Into::into)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn child_navigation_preserves_displayed_revision_and_pages_exact_child() {
        let run = ProgramRunId::new("program-navigation").unwrap();
        for ordinal in 1..=rsi_agent_session_protocol::MAXIMUM_PROGRAM_CHILDREN {
            let input = Input::Child {
                request: child_request(run.clone(), (1_u64 << 53) + 1, ordinal),
                ordinal,
            };
            let encoded = serde_json::to_value(input).unwrap();
            assert_eq!(
                encoded["request"]["expected_control_seq"],
                "9007199254740993"
            );
            let Input::Child {
                request,
                ordinal: decoded,
            } = serde_json::from_value(encoded).unwrap()
            else {
                panic!("child action")
            };
            request.validate().unwrap();
            assert_eq!(decoded, ordinal);
            assert!(request.children_offset < ordinal as usize);
            assert!(
                ordinal as usize
                    <= request.children_offset + rsi_session_protocol::WORKFLOW_CHILD_PAGE_SIZE
            );
        }
    }
}
