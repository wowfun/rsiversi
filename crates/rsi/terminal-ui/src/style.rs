//! Semantic terminal styles. Terminal-default surfaces preserve user palettes.
use crate::transcript::{ProcessOutcome, Role};
use ratatui::style::{Color, Modifier, Style};

pub(crate) const SURFACE: Color = Color::Reset;
pub(crate) const METADATA: Color = Color::Indexed(247);
pub(crate) const USER_BAND: Color = Color::Indexed(235);
pub(crate) fn role_color(role: Role) -> Color {
    match role {
        Role::User | Role::Tool => Color::Gray,
        Role::Assistant => Color::Reset,
        Role::Reasoning | Role::Status | Role::Metadata | Role::Notice => Color::DarkGray,
        Role::Error => Color::LightRed,
    }
}
pub(crate) fn outcome_color(outcome: Option<ProcessOutcome>, neutral: Color) -> Color {
    match outcome {
        Some(ProcessOutcome::Success) => Color::LightGreen,
        Some(ProcessOutcome::Failed) => Color::LightRed,
        Some(ProcessOutcome::Interrupted) | None => neutral,
    }
}
pub(crate) fn process_marker(outcome: Option<ProcessOutcome>, collapsed: bool) -> &'static str {
    match outcome {
        Some(ProcessOutcome::Success) => "✓",
        Some(ProcessOutcome::Failed) => "×",
        Some(ProcessOutcome::Interrupted) => "−",
        None if collapsed => "▸",
        None => "▾",
    }
}
pub(crate) fn selection() -> Style {
    Style::default().bg(Color::Cyan).fg(Color::Black)
}

pub(crate) fn user_style() -> Style {
    Style::default().fg(Color::Gray).bg(Color::Indexed(235))
}

pub(crate) fn muted() -> Style {
    Style::default().add_modifier(Modifier::DIM)
}
pub(crate) fn accent() -> Style {
    Style::default()
        .fg(Color::Gray)
        .add_modifier(Modifier::BOLD)
}

pub(crate) fn border() -> Style {
    Style::default().fg(Color::Indexed(239))
}

pub(crate) fn choice_style(selected: bool) -> Style {
    let style = Style::default().fg(Color::Gray).bg(Color::Indexed(235));
    if selected {
        style.bg(Color::Indexed(237)).add_modifier(Modifier::BOLD)
    } else {
        style
    }
}
