use super::*;
use std::collections::VecDeque;

const MAXIMUM_ENTRIES: usize = 100;
const MAXIMUM_BYTES: usize = 1024 * 1024;

struct Entry {
    id: u64,
    session: SessionId,
    text: Box<str>,
}
#[derive(Default)]
pub(super) struct Prompts {
    entries: VecDeque<Entry>,
    next: u64,
    bytes: usize,
}
impl Prompts {
    pub(super) fn remember(&mut self, session: &SessionId, text: &str) {
        if text.trim().is_empty()
            || text.len() > MAXIMUM_BYTES
            || self
                .entries
                .iter()
                .rev()
                .find(|entry| entry.session == *session)
                .is_some_and(|entry| entry.text.as_ref() == text)
        {
            return;
        }
        let Some(next) = self.next.checked_add(1) else {
            return;
        };
        while self.entries.len() >= MAXIMUM_ENTRIES || self.bytes + text.len() > MAXIMUM_BYTES {
            self.bytes -= self
                .entries
                .pop_front()
                .expect("nonempty retained history")
                .text
                .len();
        }
        self.next = next;
        self.bytes += text.len();
        self.entries.push_back(Entry {
            id: next,
            session: session.clone(),
            text: text.into(),
        });
    }
}
impl Client {
    pub(super) fn remember_prompt(&mut self) {
        if let Some(request) = &self.submission.request
            && let [MessageInput::Text { text }] = request.content.as_slice()
        {
            self.prompts.remember(self.state.header.session_id(), text);
        }
    }
    pub(super) fn prompt_menu(&mut self) {
        self.state.invalidate_detail();
        self.state.menu = Some(Menu {
            title: "Submitted input history".into(),
            selected: 0,
            items: self
                .prompts
                .entries
                .iter()
                .rev()
                .filter(|entry| entry.session == *self.state.header.session_id())
                .map(|entry| {
                    let prefix: String = entry.text.chars().take(80).collect();
                    let preview: String = super::terminal_text(&prefix)
                        .chars()
                        .map(|c| if c == '\n' { '↵' } else { c })
                        .collect();
                    (preview, Action::RecallPrompt(entry.id))
                })
                .collect(),
        });
    }
    pub(super) fn recall_prompt(&mut self, id: u64) {
        let entry = self
            .prompts
            .entries
            .iter()
            .find(|entry| entry.id == id && entry.session == *self.state.header.session_id());
        if let Some(entry) = entry {
            if let Err(problem) = self.state.editor.replace_text(&entry.text) {
                self.state.notice(problem);
            }
        } else {
            self.state
                .notice("This history entry is no longer available");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn session_local_history_keeps_exact_text_under_global_byte_and_entry_bounds() {
        let first = SessionId::new("first").unwrap();
        let second = SessionId::new("second").unwrap();
        let mut history = Prompts::default();
        history.remember(&first, "  first\nline  ");
        history.remember(&second, "another Session");
        history.remember(&first, "  first\nline  ");
        assert_eq!(history.entries.len(), 2);
        assert_eq!(&*history.entries[0].text, "  first\nline  ");
        for i in 0..200 {
            history.remember(&first, &i.to_string());
        }
        assert_eq!(history.entries.len(), MAXIMUM_ENTRIES);
        history.remember(&second, &"x".repeat(MAXIMUM_BYTES));
        assert_eq!(history.entries.len(), 1);
        assert_eq!(history.bytes, MAXIMUM_BYTES);
        history.remember(&first, "replacement");
        assert_eq!(history.entries.len(), 1);
        assert_eq!(history.bytes, "replacement".len());
    }
}
