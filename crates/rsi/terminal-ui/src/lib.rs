//! Pure, bounded terminal presentation. No terminal descriptors or runtime owner.
#![deny(unsafe_code)]
#![allow(clippy::missing_errors_doc)] // Bounds and error categories are owned by the package contract.
mod dialog;
pub mod editor;
mod markdown;
#[cfg(all(test, target_os = "linux"))]
mod performance;
pub mod render;
pub mod scene;
#[cfg(test)]
mod test_state;
pub mod transcript;
pub mod wire;
pub const MAX_TEXT: usize = rsi_agent_session_protocol::MAXIMUM_TURN_TEXT_BYTES;
use editor::Editor;
use rsi_agent_session_protocol::SessionHeader;
use rsi_ai_protocol::ModelRef;
use transcript::{Anchor, Transcript};
#[derive(Debug)]
pub struct Menu<'a> {
    pub title: &'a str,
    pub items: Vec<&'a str>,
    pub selected: usize,
}
#[derive(Debug)]
pub struct Answer<'a> {
    pub scroll: usize,
    pub request: &'a rsi_user_questions_protocol::QuestionRequest,
    pub answered: usize,
    pub editor: &'a Editor,
}
#[derive(Debug)]
pub struct Edit<'a> {
    pub label: &'a str,
    pub editor: &'a Editor,
}
#[derive(Debug)]
#[allow(clippy::struct_excessive_bools)] // Independent display facts, not an exclusive state machine.
pub struct Input<'a> {
    pub markdown: bool,
    pub activity: Option<Activity>,
    pub header: &'a SessionHeader,
    pub workspace_label: &'a str,
    pub transcript: &'a Transcript,
    pub editor: &'a Editor,
    pub model: Option<&'a ModelRef>,
    pub reasoning_effort: Option<&'a rsi_ai_protocol::ReasoningEffortId>,
    pub menu_revision: u64,
    pub todos: Option<&'a rsi_agent_todo::TodoList>,
    pub metrics: Option<&'a rsi_conversation::SessionMetrics>,
    pub metrics_complete: bool,
    pub model_unavailable: bool,
    pub completion: Option<&'a scene::Completion>,
    pub menu: Option<Menu<'a>>,
    pub answer: Option<Answer<'a>>,
    pub ui_edit: Option<Edit<'a>>,
    pub detail: Option<&'a str>,
    pub detail_offset: usize,
    pub selection: Option<(Anchor, Anchor)>,
    pub top: Option<Anchor>,
    pub fold_focus: Option<(Anchor, u16)>,
    pub status: &'a str,
    pub actual_model: Option<&'a str>,
    pub active: bool,
    pub busy: bool,
    pub remote: bool,
    pub questions: usize,
    pub approvals: usize,
}

/// Explicit visible-turn clock supplied by the resident; rendering never reads time.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Activity {
    pub turn_id: rsi_agent_session_protocol::TurnId,
    pub now_ms: u64,
    pub started_ms: Option<u64>,
}
impl Activity {
    pub fn elapsed_ms(&self) -> Option<u64> {
        self.started_ms
            .and_then(|start| self.now_ms.checked_sub(start))
    }
}

#[allow(clippy::missing_panics_doc)] // Modulo eight proves both conversion and array indexing.
pub fn activity_spinner_frame(elapsed_ms: u64) -> &'static str {
    const FRAMES: [&str; 8] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧"];
    FRAMES[usize::try_from((elapsed_ms / 120) % 8).expect("eight spinner frames")]
}

pub fn elapsed_label(elapsed_ms: u64) -> String {
    let seconds = elapsed_ms / 1000;
    if seconds >= 3600 {
        format!("{}h {}m", seconds / 3600, (seconds % 3600) / 60)
    } else if seconds >= 60 {
        format!("{}m {}s", seconds / 60, seconds % 60)
    } else {
        format!("{seconds}s")
    }
}
/// Neutralizes terminal and bidi controls while preserving line breaks and joiners.
pub fn terminal_text(text: &str) -> String {
    text.chars().map(terminal_character).collect()
}

pub fn terminal_character(character: char) -> char {
    if character.is_control() && !matches!(character, '\n' | '\t')
        || matches!(character, '\u{061c}' | '\u{200e}'..='\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
    {
        '\u{fffd}'
    } else {
        character
    }
}
