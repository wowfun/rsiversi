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
    fn model(&self, target: Context) -> BoxFuture<'_, Result<rsi_ui::UiModel>> {
        Box::pin(async move {
            rsi_ui::UiModel::standard(goal_view(controller(&target)?.as_ref()).await?)
                .map_err(Into::into)
        })
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

fn goal_evidence(elements: &mut Vec<UiElement>, goal: &rsi_agent_goal::Goal) {
    if let Some(reason) = &goal.reason
        && goal
            .report
            .as_ref()
            .is_none_or(|report| &report.evidence != reason)
    {
        elements.push(text(reason));
    }
    if let Some(source) = &goal.completed_by {
        let source = match source {
            rsi_agent_goal::GoalCompleter::Owner => "Host owner",
            rsi_agent_goal::GoalCompleter::Participant => "Session participant",
            rsi_agent_goal::GoalCompleter::Plugin { name } => name,
        };
        elements.push(field("Completed by", source));
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
async fn goal_view(controller: &SessionController) -> Result<UiView> {
    let cache = controller.projection_changes().borrow().clone();
    let live = controller.goal_changes().borrow().clone();
    let control = controller.goal_control_changes().borrow().clone();
    let mut elements = Vec::new();
    if let rsi_client::GoalControlState::Rejected {
        request_id,
        diagnostic,
    } = &control
    {
        elements.push(text(format!(
            "Goal control rejected. Request {request_id}: {diagnostic}"
        )));
    }
    let armed = match live {
        Some(Ok(live)) => {
            elements.push(field(
                "Current driving",
                if live.armed { "Armed" } else { "Disarmed" },
            ));
            elements.push(field("Driver", format!("{:?}", live.stage)));
            if let Some(detail) = live.detail {
                elements.push(text(detail));
            }
            live.available.then_some(live.armed)
        }
        Some(Err(error)) => {
            elements.push(text(format!("Live Goal state unavailable: {error}")));
            None
        }
        None => {
            elements.push(text("Loading live Goal state…"));
            None
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
    let state = GoalState::deserialize(value.value()).map_err(invalid)?;
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
        if goal.phase == GoalPhase::Paused && goal.unsettled() {
            elements.push(text("The allocated round is still settling."));
        }
        goal_evidence(&mut elements, goal);
    }
    if let rsi_client::GoalControlState::Pending(pending) = &control {
        elements.push(text(format!("Control outcome is unresolved. Request {}. Check its receipt before sending another control.", pending.request_id)));
        elements.push(goal_button("Check control result", GoalInput::Reconcile));
        return Ok(view("Goal", elements));
    }
    let Some(armed) = armed else {
        return Ok(view("Goal", elements));
    };
    if let Some(goal) = &state.goal {
        let mut control = |label: &str, action| -> Result<()> {
            elements.push(goal_button(
                label,
                GoalInput::Control {
                    request: GoalControl {
                        published_at_ms: None,
                        request_id: identity()?,
                        expected_revision: revision,
                        action,
                    },
                },
            ));
            Ok(())
        };
        if goal.phase != GoalPhase::Completed {
            if goal.phase == GoalPhase::Active && armed {
                control(
                    "Pause after current round",
                    Control::Pause {
                        id: goal.id.clone(),
                    },
                )?;
            } else if goal.has_round_capacity() {
                control(
                    "Resume Goal",
                    Control::Resume {
                        id: goal.id.clone(),
                    },
                )?;
            }
            let owner = controller.is_host_owner().await.unwrap_or(false);
            control(
                "Mark complete",
                Control::Complete {
                    id: goal.id.clone(),
                    source: if owner {
                        rsi_agent_goal::GoalCompleter::Owner
                    } else {
                        rsi_agent_goal::GoalCompleter::Participant
                    },
                },
            )?;
            if owner {
                control(
                    "Cancel automatic round",
                    Control::Cancel {
                        id: goal.id.clone(),
                    },
                )?;
            }
        }
        control(
            "Clear Goal",
            Control::Clear {
                id: goal.id.clone(),
            },
        )?;
    }
    if state
        .goal
        .as_ref()
        .is_none_or(|goal| goal.phase == GoalPhase::Completed)
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
    Ok(view("Goal", elements))
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
            let mut control_requested = false;
            let result: Result<_> = async {
                let request: GoalInput = serde_json::from_value(input.value).map_err(invalid)?;
                match request {
                    GoalInput::Reconcile => {
                        if !input.fields.is_empty() {
                            return Err(invalid("Receipt checking accepts no fields"));
                        }
                        control_requested = true;
                        controller.reconcile_goal().await
                    }
                    GoalInput::Control { request } => {
                        if input.fields.keys().any(|key| {
                            !["objective", "constraints", "rounds"].contains(&key.as_str())
                        }) {
                            return Err(invalid("Unexpected Goal form field"));
                        }
                        control_requested = true;
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
                        control_requested = true;
                        controller
                            .control_goal(GoalControl {
                                published_at_ms: None,
                                request_id: request,
                                expected_revision: revision,
                                action: Control::Create {
                                    id,
                                    objective: input.fields["objective"].clone(),
                                    constraints: input.fields["constraints"].clone(),
                                    max_rounds: Some(max_rounds),
                                },
                            })
                            .await
                    }
                }
                .map_err(invalid)
            }
            .await;
            let mut result_view = goal_view(&controller).await?;
            if let Err(error) = result
                && (!control_requested
                    || matches!(
                        *controller.goal_control_changes().borrow(),
                        rsi_client::GoalControlState::Idle
                    ))
            {
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

#[cfg(test)]
mod goal_card_tests {
    use super::*;
    use rsi_agent_goal::{GoalReport, GoalReportKind, RoundOutcome, RoundSettlement};
    use rsi_agent_session_protocol::TurnId;

    #[test]
    fn the_card_displays_each_reason_once_and_keeps_the_model_report_source() {
        for outcome in [RoundOutcome::Completed, RoundOutcome::Failed] {
            let mut state = GoalState::default();
            state
                .apply(
                    Control::Create {
                        id: DomainRequestId::new("goal").unwrap(),
                        objective: "Review the draft".into(),
                        constraints: String::new(),
                        max_rounds: Some(2),
                    },
                    2,
                    false,
                )
                .unwrap();
            let goal = state.goal.as_mut().unwrap();
            goal.reserve().unwrap();
            let message = goal.reservation.as_ref().unwrap().message_id.clone();
            let turn = TurnId::new("review-turn").unwrap();
            goal.record_report(GoalReport {
                kind: GoalReportKind::Blocked,
                evidence: "Awaiting the editor".into(),
                source_turn: turn.clone(),
            })
            .unwrap();
            goal.settle(
                &message,
                RoundSettlement::Turn {
                    turn_id: turn,
                    outcome,
                },
            )
            .unwrap();
            let mut elements = Vec::new();
            goal_evidence(&mut elements, goal);
            let visible = serde_json::to_string(&elements).unwrap();
            assert_eq!(visible.matches("Awaiting the editor").count(), 1);
            assert!(visible.contains("Blocked · Turn review-turn"));
            assert_eq!(
                visible.contains("its model report is unverified"),
                outcome == RoundOutcome::Failed
            );
        }
    }
}
