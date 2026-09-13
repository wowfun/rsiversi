use super::{
    ActionInput, ActionTarget, BoxFuture, Context, Deserialize, FieldWindow, Result, Serialize,
    SessionController, SurfaceRenderer, UiAction, UiElement, UiError, UiView, controller,
};
use rsi_agent_goal::{GoalAction as Control, GoalPhase, GoalState};
use rsi_agent_session_protocol::{CommandRevision, DomainRequestId, ProjectionCursor};
use rsi_goal::GoalControl;

#[derive(Debug)]
pub(super) struct GoalCard;
impl SurfaceRenderer for GoalCard {
    fn render(&self, target: &Context) -> Result<UiView> {
        goal_view(controller(target)?.as_ref())
    }
}
fn text(value: impl Into<String>) -> UiElement {
    UiElement::Text { text: value.into() }
}
fn field(label: &str, value: impl Into<String>) -> UiElement {
    UiElement::Field {
        label: label.into(),
        value: value.into(),
    }
}
fn view(title: &str, elements: Vec<UiElement>) -> UiView {
    UiView {
        title: title.into(),
        elements,
    }
}
fn invalid(error: impl std::fmt::Display) -> UiError {
    UiError::Invalid(error.to_string())
}
fn identity() -> Result<DomainRequestId> {
    DomainRequestId::new(rsi_ui::fresh_identity("goal").map_err(invalid)?).map_err(invalid)
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum GoalInput {
    Create {
        request: DomainRequestId,
        id: DomainRequestId,
        revision: CommandRevision,
    },
    Control {
        request: GoalControl,
    },
    Reconcile,
}
fn goal_button(label: &str, input: GoalInput) -> UiElement {
    UiElement::Button {
        action: "goal".into(),
        label: label.into(),
        value: serde_json::to_value(input).expect("closed Goal input"),
    }
}
fn goal_view(controller: &SessionController) -> Result<UiView> {
    let cache = controller.projection_changes().borrow().clone();
    let live = controller.goal_changes().borrow().clone();
    let mut elements = Vec::new();
    let available = match live {
        Some(Ok(live)) => {
            elements.push(field(
                "Current driving",
                if live.armed { "Armed" } else { "Disarmed" },
            ));
            elements.push(field("Driver", format!("{:?}", live.stage)));
            if let Some(detail) = live.detail {
                elements.push(text(detail));
            }
            live.available
        }
        Some(Err(error)) => {
            elements.push(text(format!("Live Goal state unavailable: {error}")));
            false
        }
        None => {
            elements.push(text("Loading live Goal state…"));
            false
        }
    };
    let Some(cache) = cache else {
        elements.push(text("Loading durable Goal state…"));
        return Ok(view("Goal", elements));
    };
    let snapshot = cache.snapshot();
    let Some(entry) = snapshot
        .entries()
        .iter()
        .find(|entry| entry.producer().as_str() == rsi_agent_goal::GOAL_PROJECTION)
    else {
        elements.push(text("This preset has no Goal contribution."));
        return Ok(view("Goal", elements));
    };
    let Some(value) = entry.view() else {
        elements.push(text(
            entry.failure().unwrap_or("Goal projection unavailable"),
        ));
        return Ok(view("Goal", elements));
    };
    let state: GoalState = serde_json::from_value(value.value().clone()).map_err(invalid)?;
    state.validate().map_err(invalid)?;
    let revision = match snapshot.cursor() {
        ProjectionCursor::Draft { revision } => CommandRevision::Draft { revision },
        ProjectionCursor::Durable { control_seq, .. } => CommandRevision::Durable { control_seq },
    };
    if let Some(goal) = &state.goal {
        elements.extend([
            field("Goal", &goal.objective),
            field("Durable phase", format!("{:?}", goal.phase)),
            field(
                "Allocated rounds",
                format!("{} / {}", goal.allocated_rounds, goal.max_rounds),
            ),
        ]);
        if !goal.constraints.is_empty() {
            elements.push(field("Constraints", &goal.constraints));
        }
        if let Some(reason) = &goal.reason {
            elements.push(text(reason));
        }
        if let Some(report) = &goal.report {
            elements.push(field(
                "Model report",
                format!(
                    "{:?} · Turn {}\n{}",
                    report.kind, report.source_turn, report.evidence
                ),
            ));
        }
    }
    if let Some(pending) = controller.pending_goal() {
        elements.push(text(format!("Control outcome is unresolved. Request {}. Check its receipt before sending another control.", pending.request_id)));
        elements.push(goal_button("Check control result", GoalInput::Reconcile));
        return Ok(view("Goal", elements));
    }
    if !available {
        return Ok(view("Goal", elements));
    }
    elements.extend(goal_controls(&state, revision)?);
    Ok(view("Goal", elements))
}

fn goal_controls(state: &GoalState, revision: CommandRevision) -> Result<Vec<UiElement>> {
    let mut elements = Vec::new();
    if let Some(goal) = &state.goal {
        for (label, action) in [
            (
                "Pause after current round",
                Control::Pause {
                    id: goal.id.clone(),
                },
            ),
            (
                "Cancel automatic round",
                Control::Cancel {
                    id: goal.id.clone(),
                },
            ),
            (
                "Resume Goal",
                Control::Resume {
                    id: goal.id.clone(),
                },
            ),
        ] {
            if goal.phase == GoalPhase::Completed
                || (matches!(action, Control::Resume { .. })
                    && goal.allocated_rounds == goal.max_rounds
                    && !goal.unsettled())
            {
                continue;
            }
            elements.push(goal_button(
                label,
                GoalInput::Control {
                    request: GoalControl {
                        request_id: identity()?,
                        expected_revision: revision,
                        action,
                    },
                },
            ));
        }
    }
    if state
        .goal
        .as_ref()
        .is_none_or(|goal| goal.phase != GoalPhase::Active && !goal.unsettled())
    {
        for (name, label, multiline) in [
            ("objective", "Goal objective", true),
            ("constraints", "Constraints", true),
            ("rounds", "Maximum automatic rounds", false),
        ] {
            elements.push(UiElement::Input {
                name: name.into(),
                label: label.into(),
                value: String::new(),
                multiline,
            });
        }
        elements.push(text("Enter a positive round cap. Allocations are charged before execution and are not refunded. This allowance covers automatic parent Turns."));
        elements.push(goal_button(
            "Create and start Goal",
            GoalInput::Create {
                request: identity()?,
                id: identity()?,
                revision,
            },
        ));
    }
    Ok(elements)
}

#[derive(Debug)]
pub(super) struct GoalAction;
impl UiAction for GoalAction {
    fn invoke(
        &self,
        target: ActionTarget,
        input: ActionInput,
    ) -> BoxFuture<'static, Result<UiView>> {
        Box::pin(async move {
            let controller = controller(target.context())?;
            let result: Result<_> = async {
                let request: GoalInput = serde_json::from_value(input.value).map_err(invalid)?;
                match request {
                    GoalInput::Reconcile => {
                        if !input.fields.is_empty() {
                            return Err(invalid("Receipt checking accepts no fields"));
                        }
                        controller.reconcile_goal().await
                    }
                    GoalInput::Control { request } => {
                        if input.fields.keys().any(|key| {
                            !["objective", "constraints", "rounds"].contains(&key.as_str())
                        }) {
                            return Err(invalid("Unexpected Goal form field"));
                        }
                        controller.control_goal(request).await
                    }
                    GoalInput::Create {
                        request,
                        id,
                        revision,
                    } => {
                        if input.fields.len() != 3
                            || input.fields.keys().any(|key| {
                                !["objective", "constraints", "rounds"].contains(&key.as_str())
                            })
                        {
                            return Err(invalid(
                                "Goal creation requires objective, constraints and rounds",
                            ));
                        }
                        let rounds = input.fields["rounds"].trim();
                        let max_rounds = rounds
                            .parse::<u64>()
                            .ok()
                            .filter(|value| *value > 0 && value.to_string() == rounds)
                            .ok_or_else(|| {
                                invalid("Maximum automatic rounds must be a positive integer")
                            })?;
                        controller
                            .control_goal(GoalControl {
                                request_id: request,
                                expected_revision: revision,
                                action: Control::Create {
                                    id,
                                    objective: input.fields["objective"].clone(),
                                    constraints: input.fields["constraints"].clone(),
                                    max_rounds,
                                },
                            })
                            .await
                    }
                }
                .map_err(invalid)
            }
            .await;
            let mut result_view = goal_view(&controller)?;
            if let Err(error) = result {
                result_view
                    .elements
                    .insert(0, text(format!("Goal control: {error}")));
            }
            Ok(result_view)
        })
    }
}

#[derive(Debug)]
pub(super) struct JobsCard;
impl SurfaceRenderer for JobsCard {
    fn model(&self, target: Context) -> BoxFuture<'_, Result<rsi_ui::UiModel>> {
        Box::pin(async move {
            Ok(rsi_ui::UiModel::standard(
                jobs_view(controller(&target)?.as_ref(), None).await?,
            )?)
        })
    }
}
fn jobs_button(label: &str, page: Option<rsi_agent_turn_protocol::TurnJobsRequest>) -> UiElement {
    UiElement::Button {
        action: "jobs".into(),
        label: label.into(),
        value: serde_json::to_value(page).expect("closed Jobs input"),
    }
}
async fn jobs_view(
    controller: &SessionController,
    request: Option<rsi_agent_turn_protocol::TurnJobsRequest>,
) -> Result<UiView> {
    let mut elements = vec![text(
        "Current active Turn only. Status is process-local; reading it does not report, wait for or control a job.",
    )];
    match controller.jobs(request).await {
        Ok(Some(snapshot)) => {
            let page = snapshot.page();
            elements.push(field("Turn", page.turn_id.to_string()));
            elements.push(field("Scope generation", page.generation.to_string()));
            if page.jobs.is_empty() {
                elements.push(text("No jobs in this active Turn."));
            }
            for job in &page.jobs {
                let mut row = format!(
                    "{} · {:?}\n{} · {}\nReported: {} · Output retained: {}",
                    job.name, job.status, job.id, job.producer, job.reported, job.output_retained
                );
                if let Some(terminal) = &job.terminal {
                    let value = serde_json::to_string(terminal).map_err(invalid)?;
                    let preview = FieldWindow::text(&value, 0, 512).map_err(invalid)?;
                    row.push('\n');
                    row.push_str(&preview.text);
                    if preview.more {
                        row.push_str(" … (diagnostic shortened)");
                    }
                }
                elements.push(UiElement::Code { text: row });
            }
            if page.has_more {
                elements.push(jobs_button(
                    "Next Jobs page",
                    Some(rsi_agent_turn_protocol::TurnJobsRequest {
                        turn_id: page.turn_id.clone(),
                        generation: Some(page.generation),
                        after: page.jobs.last().map(|job| job.id.clone()),
                        limit: 16,
                    }),
                ));
            }
        }
        Ok(None) => elements.push(text(
            "No active Turn. Jobs are unavailable after their Turn finishes.",
        )),
        Err(error) => elements.push(text(format!("Current-Turn Jobs unavailable: {error}"))),
    }
    elements.push(jobs_button("Refresh current Turn", None));
    Ok(view("Current-Turn Jobs", elements))
}
#[derive(Debug)]
pub(super) struct JobsAction;
impl UiAction for JobsAction {
    fn invoke(
        &self,
        target: ActionTarget,
        input: ActionInput,
    ) -> BoxFuture<'static, Result<UiView>> {
        Box::pin(async move {
            if !input.fields.is_empty() {
                return Err(invalid("Jobs status accepts no form fields"));
            }
            let request = serde_json::from_value(input.value).map_err(invalid)?;
            tokio::select! { biased;
                () = target.cancelled() => Err(UiError::Retired),
                () = target.view_closed() => Err(UiError::Retired),
                result = async { jobs_view(controller(target.context())?.as_ref(), request).await } => result,
            }
        })
    }
}
