//! Pure, bounded terminal presentation. No terminal descriptors or runtime owner.
#![deny(unsafe_code)]
#![allow(clippy::missing_errors_doc)] // Bounds and error categories are owned by the package contract.
pub mod editor;
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
    pub header: &'a SessionHeader,
    pub transcript: &'a Transcript,
    pub editor: &'a Editor,
    pub model: Option<&'a ModelRef>,
    pub enter_submit: bool,
    pub menu: Option<Menu<'a>>,
    pub answer: Option<Answer<'a>>,
    pub ui_edit: Option<Edit<'a>>,
    pub detail: Option<&'a str>,
    pub detail_offset: usize,
    pub selection: Option<(Anchor, Anchor)>,
    pub top: Option<Anchor>,
    pub status: &'a str,
    pub actual_model: Option<&'a str>,
    pub active: bool,
    pub busy: bool,
    pub remote: bool,
    pub questions: usize,
    pub approvals: usize,
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
