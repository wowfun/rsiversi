//! Terminal control sequences are withheld until complete and capped before parsing.
const CAP: usize = 8192;
#[derive(Debug, Clone, Copy, Default)]
enum State {
    #[default]
    Ground,
    Escape,
    Csi,
    String {
        osc: bool,
        escape: bool,
    },
}
#[derive(Debug, Default)]
pub struct Filter {
    state: State,
    pending: Vec<u8>,
    oversized: bool,
    utf8: Vec<u8>,
}
impl Filter {
    pub fn feed(&mut self, bytes: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        let mut utf8 = std::mem::take(&mut self.utf8);
        // Bound the decoder's transient prefix even when its caller batches output.
        for chunk in bytes.chunks(8192) {
            utf8.extend_from_slice(chunk);
            let mut offset = 0;
            loop {
                match std::str::from_utf8(&utf8[offset..]) {
                    Ok(text) => {
                        self.text(text, &mut out);
                        offset = utf8.len();
                        break;
                    }
                    Err(error) => {
                        let end = offset + error.valid_up_to();
                        let text = std::str::from_utf8(&utf8[offset..end])
                            .expect("validated UTF-8 prefix");
                        self.text(text, &mut out);
                        offset = end;
                        if let Some(length) = error.error_len() {
                            self.text("�", &mut out);
                            offset += length;
                        } else {
                            break;
                        }
                    }
                }
            }
            utf8.drain(..offset);
        }
        self.utf8 = utf8;
        out
    }
    fn text(&mut self, text: &str, out: &mut Vec<u8>) {
        for character in text.chars() {
            // Normalize C1 controls so the Rust and browser parsers see the same grammar.
            let replacement = match character {
                '\u{90}' => Some(b'P'),
                '\u{98}' => Some(b'X'),
                '\u{9b}' => Some(b'['),
                '\u{9c}' => Some(b'\\'),
                '\u{9d}' => Some(b']'),
                '\u{9e}' => Some(b'^'),
                '\u{9f}' => Some(b'_'),
                _ => None,
            };
            if let Some(next) = replacement {
                self.byte(0x1b, out);
                self.byte(next, out);
            } else if !('\u{80}'..='\u{9f}').contains(&character) {
                let mut encoded = [0; 4];
                for byte in character.encode_utf8(&mut encoded).bytes() {
                    self.byte(byte, out);
                }
            }
        }
    }
    fn keep(&mut self, byte: u8) {
        if self.oversized {
            return;
        }
        if self.pending.len() == CAP {
            self.pending.clear();
            self.oversized = true;
        } else {
            self.pending.push(byte);
        }
    }
    fn finish(&mut self, out: &mut Vec<u8>) {
        if !self.oversized && !matches!(self.state, State::String { .. }) {
            out.append(&mut self.pending);
        }
        self.pending.clear();
        self.oversized = false;
        self.state = State::Ground;
    }
    fn byte(&mut self, byte: u8, out: &mut Vec<u8>) {
        if matches!(byte, 0x18 | 0x1a) {
            self.pending.clear();
            self.oversized = false;
            self.state = State::Ground;
            return;
        }
        match self.state {
            State::Ground => {
                if byte == 0x1b {
                    self.keep(byte);
                    self.state = State::Escape;
                } else {
                    out.push(byte);
                }
            }
            State::Escape => {
                if byte == 0x1b {
                    self.pending.clear();
                    self.oversized = false;
                    self.keep(byte);
                    return;
                }
                self.keep(byte);
                match byte {
                    b'[' => self.state = State::Csi,
                    b']' | b'P' | b'X' | b'^' | b'_' => {
                        self.state = State::String {
                            osc: byte == b']',
                            escape: false,
                        }
                    }
                    0x30..=0x7e => self.finish(out),
                    _ => {}
                }
            }
            State::Csi => {
                if byte == 0x1b {
                    self.pending.clear();
                    self.oversized = false;
                    self.keep(byte);
                    self.state = State::Escape;
                    return;
                }
                self.keep(byte);
                if (0x40..=0x7e).contains(&byte) {
                    self.finish(out);
                }
            }
            State::String { osc, escape } => {
                self.keep(byte);
                if (osc && byte == 7) || (escape && byte == b'\\') {
                    self.finish(out);
                } else {
                    self.state = State::String {
                        osc,
                        escape: byte == 0x1b,
                    };
                }
            }
        }
    }
    #[cfg(test)]
    pub fn retained(&self) -> usize {
        self.pending.len() + self.utf8.len()
    }
}

#[cfg(test)]
mod tests {
    use super::Filter;
    #[test]
    fn huge_unterminated_controls_are_bounded_before_both_consumers() {
        for introducer in ["\x1b]0;", "\x1bP", "\u{9d}0;"] {
            let mut filter = Filter::default();
            let mut parser = vt100::Parser::new(24, 80, 1000);
            assert!(filter.feed(introducer.as_bytes()).is_empty());
            for _ in 0..8192 {
                let bytes = filter.feed(&[b'x'; 1024]);
                assert!(bytes.is_empty());
                parser.process(&bytes);
                assert!(filter.retained() <= 8195);
            }
            let output = filter.feed(b"\x1b\\OK");
            parser.process(&output);
            assert_eq!(output, b"OK");
            assert_eq!(parser.screen().contents(), "OK");
        }
    }
    #[test]
    fn utf8_width_combining_alt_screen_resize_and_visible_snapshot() {
        let mut filter = Filter::default();
        let mut parser = vt100::Parser::new(24, 80, 1000);
        let script = "\x1b[31m界e\u{301}\x1b[0m\r\nsecond\x1b[?1049hALT\x1b[?1049l\x1b[3;4Hcursor";
        for byte in script.bytes() {
            parser.process(&filter.feed(&[byte]));
        }
        parser.screen_mut().set_size(40, 120);
        assert!(parser.screen().contents().contains("界e\u{301}"));
        assert!(!parser.screen().contents().contains("ALT"));
        let snapshot = parser.screen().state_formatted();
        let mut restored = vt100::Parser::new(40, 120, 1000);
        restored.process(&snapshot);
        assert_eq!(restored.screen().contents(), parser.screen().contents());
        assert_eq!(
            restored.screen().cursor_position(),
            parser.screen().cursor_position()
        );
    }
    #[test]
    fn exact_limit_and_split_terminators() {
        let mut filter = Filter::default();
        let mut sequence = b"\x1b[".to_vec();
        sequence.resize(8191, b'0');
        sequence.push(b'm');
        assert_eq!(filter.feed(&sequence), sequence);
        let mut oversized = b"\x1b[".to_vec();
        oversized.resize(8192, b'0');
        assert!(filter.feed(&oversized).is_empty());
        assert_eq!(filter.feed(b"mtext"), b"text");
    }
    #[test]
    fn application_strings_never_reach_either_parser_at_any_partition() {
        for sequence in [
            "\x1b]52;c;YXR0YWNr\x07",
            "\x1b]8;;https://evil.test\x1b\\",
            "\x1b]4;1;rgb:ff/00/00\x07",
            "\x1b]10;red\x07",
            "\x1b]11;red\x07",
            "\x1b]0;title\x07",
            "\x1bPdata\x1b\\",
            "\x1b_data\x1b\\",
            "\x1b^data\x1b\\",
            "\x1bXdata\x1b\\",
            "\u{9d}8;;https://evil.test\u{9c}",
        ] {
            for split in 0..=sequence.len() {
                let mut filter = Filter::default();
                let mut output = filter.feed(&sequence.as_bytes()[..split]);
                output.extend(filter.feed(&sequence.as_bytes()[split..]));
                output.extend(filter.feed(b"visible"));
                assert_eq!(output, b"visible", "sequence {sequence:?}, split {split}");
            }
        }
    }
}
