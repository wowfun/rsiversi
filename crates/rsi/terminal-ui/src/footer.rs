use super::{FooterAction, ellipsis};
use crate::Input;
use ratatui::layout::Rect;
use unicode_width::UnicodeWidthStr as _;

pub(super) struct Footer {
    pub(super) text: String,
    pub(super) actions: Vec<(Rect, FooterAction)>,
}
#[allow(clippy::too_many_lines)] // One priority-ordered width budget owns labels and their actionable spans.
pub(super) fn build(state: &Input<'_>, width: u16, y: u16) -> Footer {
    let capacity = usize::from(width);
    let mut text = String::new();
    let mut actions = Vec::new();
    if state.header.fork_origin().is_some() {
        text.push_str("↑ ");
        actions.push((Rect::new(0, y, 1, 1), FooterAction::Parent));
    }
    let x = text.width();
    let activity = state.activity.as_ref().map(|activity| {
        let spinner =
            crate::activity_spinner_frame(activity.elapsed_ms().unwrap_or(activity.now_ms));
        activity.elapsed_ms().map_or_else(
            || spinner.into(),
            |ms| format!("{spinner} {}", crate::elapsed_label(ms)),
        )
    });
    let available = capacity.saturating_sub(
        x + activity
            .as_ref()
            .map_or(0, |label: &String| label.width() + 3),
    );
    let model = state
        .model
        .unwrap_or(state.header.settings().default_model());
    let group = if state.model_unavailable {
        format!(
            "{} unavailable",
            ellipsis(model.model(), available.saturating_sub(12).max(1))
        )
    } else {
        let effort = state
            .reasoning_effort
            .map_or("default", |effort| effort.as_str());
        let effort_width = effort.width().min(available.saturating_sub(6).min(12));
        format!(
            "{} · {}",
            ellipsis(model.model(), available.saturating_sub(effort_width + 3)),
            ellipsis(effort, effort_width)
        )
    };
    let group = ellipsis(&group, available);
    if !group.is_empty() {
        actions.push((
            Rect::new(
                u16::try_from(x).expect("footer lies within u16 viewport"),
                y,
                u16::try_from(group.width()).expect("group fits viewport"),
                1,
            ),
            FooterAction::Model,
        ));
    }
    text.push_str(&group);
    if let Some(activity) = activity {
        text.push_str(" · ");
        text.push_str(&activity);
    }
    let mut optional = Vec::new();
    if let Some(metrics) = state.metrics {
        if let Some(context) = &metrics.last_context
            && context.description.model() == model
        {
            optional.push(format!(
                "last {}/{} ctx",
                context.input_tokens,
                context.input_capacity()
            ));
        }
        if metrics.reported_attempts > 0 {
            optional.push(format!(
                "{}{} toks",
                if state.metrics_complete && metrics.reported_attempts == metrics.attempts {
                    ""
                } else {
                    "≥"
                },
                metrics.tokens.input_tokens() + metrics.tokens.output_tokens()
            ));
        }
        if !state.header.settings().pricing().is_empty() {
            let cost = &metrics.configured_cost;
            optional.push(if cost.totals.is_empty() {
                "configured cost unknown".into()
            } else {
                format!(
                    "{}{} configured",
                    if state.metrics_complete && cost.is_complete() {
                        ""
                    } else {
                        "≥"
                    },
                    cost.totals
                        .iter()
                        .map(rsi_conversation::CurrencyCost::display)
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            });
        }
    }
    optional.push(crate::terminal_text(state.workspace_label));
    for value in optional {
        if text.width() + value.width() + 3 > capacity {
            break;
        }
        text.push_str(" · ");
        text.push_str(&value);
    }
    Footer { text, actions }
}
