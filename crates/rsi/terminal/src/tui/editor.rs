use super::input::MAX_TEXT;
use termina::event::{KeyCode, KeyEvent, Modifiers};
use unicode_segmentation::UnicodeSegmentation as _;

#[derive(Clone, Debug)]
pub(super) struct Editor {
    pub(super) text: String,
    pub(super) cursor: usize,
    pub(super) limit: usize,
}

impl Default for Editor {
    fn default() -> Self {
        Self {
            text: String::new(),
            cursor: 0,
            limit: MAX_TEXT,
        }
    }
}

impl Editor {
    pub(super) fn insert(&mut self, text: &str) -> Result<(), &'static str> {
        if text.contains(['\0', '\u{7f}']) {
            return Err("Input contains NUL or DEL; nothing was inserted");
        }
        let normalized = text.replace("\r\n", "\n").replace('\r', "\n");
        if self.text.len().saturating_add(normalized.len()) > self.limit.min(MAX_TEXT) {
            return Err("Draft exceeds 1 MiB; nothing was inserted");
        }
        self.text.reserve_exact(normalized.len());
        self.text.insert_str(self.cursor, &normalized);
        self.cursor += normalized.len();
        // Inserting before a combining mark can merge the next source segment.
        self.cursor = self
            .text
            .grapheme_indices(true)
            .map(|(offset, _)| offset)
            .find(|offset| *offset >= self.cursor)
            .unwrap_or(self.text.len());
        Ok(())
    }

    pub(super) fn take(&mut self) -> String {
        self.cursor = 0;
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
            KeyCode::Char('j') if control => self.insert("\n")?,
            KeyCode::Enter if key.modifiers.contains(Modifiers::SHIFT) => self.insert("\n")?,
            KeyCode::Char('a') if control => self.cursor = 0,
            KeyCode::Char('e') if control => self.cursor = self.text.len(),
            KeyCode::Char('u') if control => {
                self.text.drain(..self.cursor);
                self.cursor = 0;
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
                self.text.drain(start..self.cursor);
                self.cursor = start;
            }
            KeyCode::Delete => {
                self.text.drain(self.cursor..self.next());
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
