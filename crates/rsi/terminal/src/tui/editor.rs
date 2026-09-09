use super::input::MAX_TEXT;
use termina::event::{KeyCode, KeyEvent, Modifiers};
use unicode_segmentation::UnicodeSegmentation as _;

mod journal;
use journal::{Change, Journal};

#[derive(Clone, Debug)]
pub(super) struct Editor {
    pub(super) text: String,
    pub(super) cursor: usize,
    pub(super) limit: usize,
    retention_limit: usize,
    journal: Journal,
}

impl Default for Editor {
    fn default() -> Self {
        Self {
            text: String::new(),
            cursor: 0,
            limit: MAX_TEXT,
            retention_limit: MAX_TEXT + journal::MAXIMUM_BYTES,
            journal: Journal::default(),
        }
    }
}

impl Editor {
    pub(super) fn with_text(text: String, limit: usize) -> Self {
        Self {
            cursor: text.len(),
            text,
            limit,
            ..Self::default()
        }
    }
    pub(super) fn retained_bytes(&self) -> usize {
        self.text.capacity() + self.journal.bytes
    }
    pub(super) fn has_edits(&self) -> bool {
        !self.journal.undo.is_empty() || !self.journal.redo.is_empty()
    }
    pub(super) fn set_retention_limit(&mut self, budget: usize) {
        self.retention_limit = budget.min(MAX_TEXT + journal::MAXIMUM_BYTES);
        if self.text.capacity() > self.retention_limit {
            self.text.shrink_to_fit();
        }
        self.journal
            .trim(self.retention_limit.saturating_sub(self.text.capacity()));
    }
    pub(super) fn insert(&mut self, text: &str) -> Result<(), &'static str> {
        if text.contains(['\0', '\u{7f}']) {
            return Err("Input contains NUL or DEL; nothing was inserted");
        }
        let normalized = text.replace("\r\n", "\n").replace('\r', "\n");
        if self.text.len().saturating_add(normalized.len())
            > self.limit.min(MAX_TEXT).min(self.retention_limit)
        {
            return Err("Input exceeds its size limit; nothing was inserted");
        }
        self.replace(self.cursor..self.cursor, &normalized);
        Ok(())
    }

    pub(super) fn replace_text(&mut self, text: &str) -> Result<(), &'static str> {
        if text.contains(['\0', '\u{7f}']) {
            return Err("Input contains NUL or DEL; nothing was inserted");
        }
        if text.len() > self.limit.min(MAX_TEXT).min(self.retention_limit) {
            return Err("Input exceeds its size limit; nothing was inserted");
        }
        self.replace(0..self.text.len(), text);
        Ok(())
    }

    fn replace(&mut self, range: std::ops::Range<usize>, value: &str) {
        if self.text[range.clone()] == *value {
            return;
        }
        let mut change = Change {
            start: range.start,
            removed: self.text[range.clone()].into(),
            inserted: value.into(),
            before: self.cursor,
            after: range.start + value.len(),
        };
        self.text
            .reserve_exact(value.len().saturating_sub(range.len()));
        self.text.replace_range(range, value);
        // Inserting before a combining mark can merge the next source segment.
        change.after = self
            .text
            .grapheme_indices(true)
            .map(|(offset, _)| offset)
            .find(|offset| *offset >= change.after)
            .unwrap_or(self.text.len());
        self.cursor = change.after;
        self.journal.record(
            change,
            self.retention_limit.saturating_sub(self.text.capacity()),
        );
    }

    fn travel(&mut self, redo: bool) {
        let change = if redo {
            self.journal.redo.pop()
        } else {
            self.journal.undo.pop_back()
        };
        let Some(change) = change else {
            return;
        };
        let (remove, value, cursor) = if redo {
            (change.removed.len(), change.inserted.as_ref(), change.after)
        } else {
            (
                change.inserted.len(),
                change.removed.as_ref(),
                change.before,
            )
        };
        self.text.reserve_exact(value.len().saturating_sub(remove));
        self.text
            .replace_range(change.start..change.start + remove, value);
        self.cursor = cursor;
        if redo {
            self.journal.undo.push_back(change);
        } else {
            self.journal.redo.push(change);
        }
        self.journal
            .trim(self.retention_limit.saturating_sub(self.text.capacity()));
    }

    pub(super) fn take(&mut self) -> String {
        self.cursor = 0;
        self.journal.clear();
        std::mem::take(&mut self.text)
    }

    fn previous(&self) -> usize {
        self.text[..self.cursor]
            .grapheme_indices(true)
            .next_back()
            .map_or(0, |(i, _)| i)
    }

    fn next(&self) -> usize {
        self.text[self.cursor..]
            .graphemes(true)
            .next()
            .map_or(self.cursor, |s| self.cursor + s.len())
    }

    pub(super) fn key(&mut self, key: KeyEvent) -> Result<(), &'static str> {
        let control = key.modifiers.contains(Modifiers::CONTROL);
        match key.code {
            KeyCode::Char('z' | 'Z') if control || key.modifiers.contains(Modifiers::ALT) => {
                self.travel(key.modifiers.intersects(Modifiers::ALT | Modifiers::SHIFT));
            }
            KeyCode::Char('j') if control => self.insert("\n")?,
            KeyCode::Enter if key.modifiers.contains(Modifiers::SHIFT) => self.insert("\n")?,
            KeyCode::Char('a') if control => self.cursor = 0,
            KeyCode::Char('e') if control => self.cursor = self.text.len(),
            KeyCode::Char('u') if control => {
                self.replace(0..self.cursor, "");
            }
            KeyCode::Left => self.cursor = self.previous(),
            KeyCode::Right => self.cursor = self.next(),
            KeyCode::Up | KeyCode::Down => {
                let start = self.text[..self.cursor].rfind('\n').map_or(0, |i| i + 1);
                let column = self.text[start..self.cursor].graphemes(true).count();
                let range = if key.code == KeyCode::Up {
                    start
                        .checked_sub(1)
                        .map(|end| (self.text[..end].rfind('\n').map_or(0, |i| i + 1), end))
                } else {
                    self.text[self.cursor..].find('\n').map(|offset| {
                        let start = self.cursor + offset + 1;
                        (
                            start,
                            start
                                + self.text[start..]
                                    .find('\n')
                                    .unwrap_or(self.text.len() - start),
                        )
                    })
                };
                if let Some((start, end)) = range {
                    self.cursor = start
                        + self.text[start..end]
                            .grapheme_indices(true)
                            .nth(column)
                            .map_or(end - start, |(offset, _)| offset);
                }
            }
            KeyCode::Home => {
                self.cursor = self.text[..self.cursor].rfind('\n').map_or(0, |i| i + 1);
            }
            KeyCode::End => {
                self.cursor += self.text[self.cursor..]
                    .find('\n')
                    .unwrap_or(self.text.len() - self.cursor);
            }
            KeyCode::Backspace => {
                let start = self.previous();
                self.replace(start..self.cursor, "");
            }
            KeyCode::Delete => {
                self.replace(self.cursor..self.next(), "");
            }
            KeyCode::Char(character) if !control && !key.modifiers.contains(Modifiers::ALT) => {
                self.insert(character.encode_utf8(&mut [0; 4]))?;
            }
            _ => {}
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shortcut(code: char, modifiers: Modifiers) -> KeyEvent {
        let mut key: KeyEvent = KeyCode::Char(code).into();
        key.modifiers = modifiers;
        key
    }

    #[test]
    fn undo_redo_preserves_atomic_paste_graphemes_and_new_branch() {
        let mut editor = Editor::default();
        let text = "界e\u{301}👩🏽‍💻\nnext";
        editor.insert(text).unwrap();
        editor.key(KeyCode::Home.into()).unwrap();
        editor.key(KeyCode::Backspace.into()).unwrap();
        assert_eq!(editor.text, "界e\u{301}👩🏽‍💻next");
        editor.key(shortcut('z', Modifiers::CONTROL)).unwrap();
        assert_eq!(editor.text, text);
        assert_eq!(editor.cursor, text.len() - 4);
        editor.key(shortcut('z', Modifiers::CONTROL)).unwrap();
        assert_eq!(editor.text, "");
        editor.key(shortcut('z', Modifiers::ALT)).unwrap();
        assert_eq!(editor.text, text);
        editor
            .key(shortcut('z', Modifiers::CONTROL | Modifiers::SHIFT))
            .unwrap();
        assert_eq!(editor.text, "界e\u{301}👩🏽‍💻next");
        editor.key(shortcut('z', Modifiers::CONTROL)).unwrap();
        editor.insert("new").unwrap();
        let branch = editor.text.clone();
        editor.key(shortcut('z', Modifiers::ALT)).unwrap();
        assert_eq!(editor.text, branch);
    }

    #[test]
    fn rejected_insert_preserves_undo_and_submission_clears_it() {
        let mut editor = Editor::default();
        editor.insert("first").unwrap();
        assert!(editor.insert("\0").is_err());
        assert!(editor.insert(&"x".repeat(MAX_TEXT)).is_err());
        editor.key(shortcut('z', Modifiers::CONTROL)).unwrap();
        assert!(editor.text.is_empty());
        editor.key(shortcut('z', Modifiers::ALT)).unwrap();
        assert_eq!(editor.take(), "first");
        editor.key(shortcut('z', Modifiers::CONTROL)).unwrap();
        editor.key(shortcut('z', Modifiers::ALT)).unwrap();
        assert!(editor.text.is_empty());
    }

    #[test]
    fn journal_entry_byte_and_saved_draft_budgets_bound_both_directions() {
        let mut editor = Editor::default();
        for _ in 0..300 {
            editor.insert("x").unwrap();
        }
        assert_eq!(editor.journal.undo.len(), journal::MAXIMUM_CHANGES);
        for _ in 0..400 {
            editor.travel(false);
        }
        assert_eq!(editor.text.len(), 300 - journal::MAXIMUM_CHANGES);
        assert_eq!(editor.journal.redo.len(), journal::MAXIMUM_CHANGES);
        editor.set_retention_limit(editor.text.len() + 16);
        assert!(editor.retained_bytes() <= editor.text.len() + 16);
        for _ in 0..400 {
            editor.travel(true);
        }
        assert!(editor.retained_bytes() <= 300 - journal::MAXIMUM_CHANGES + 16);

        let mut editor = Editor::default();
        for _ in 0..8 {
            editor.insert(&"a".repeat(MAX_TEXT / 4)).unwrap();
            editor.key(shortcut('u', Modifiers::CONTROL)).unwrap();
            assert!(editor.journal.bytes <= journal::MAXIMUM_BYTES);
            assert!(editor.retained_bytes() <= MAX_TEXT + journal::MAXIMUM_BYTES);
        }
        let mut second = Editor::default();
        second.set_retention_limit(2 * MAX_TEXT - editor.retained_bytes());
        second.insert(&"b".repeat(MAX_TEXT / 2)).unwrap();
        for _ in 0..8 {
            second.travel(false);
            assert!(editor.retained_bytes() + second.retained_bytes() <= 2 * MAX_TEXT);
            second.travel(true);
            assert!(editor.retained_bytes() + second.retained_bytes() <= 2 * MAX_TEXT);
        }
    }

    #[test]
    fn inserting_a_combining_mark_undoes_to_exact_text_and_cursor() {
        let mut editor = Editor::default();
        editor.insert("e中").unwrap();
        editor.key(KeyCode::Left.into()).unwrap();
        editor.insert("\u{301}").unwrap();
        assert_eq!(editor.cursor, 3);
        editor.travel(false);
        assert_eq!((&*editor.text, editor.cursor), ("e中", 1));
        editor.travel(true);
        assert_eq!((&*editor.text, editor.cursor), ("e\u{301}中", 3));
    }
    #[test]
    fn grapheme_editing_and_rejected_paste_preserve_draft() {
        let mut editor = Editor::default();
        editor.insert("界e\u{301}👩🏽‍💻").unwrap();
        editor.key(KeyCode::Backspace.into()).unwrap();
        assert_eq!(editor.text, "界e\u{301}");
        editor.key(KeyCode::Backspace.into()).unwrap();
        assert_eq!(editor.text, "界");
        assert!(editor.insert(&"a".repeat(MAX_TEXT)).is_err());
        assert_eq!(editor.text, "界");
        assert!(editor.insert("\0").is_err());
        assert_eq!(editor.cursor, 3);
    }
}
