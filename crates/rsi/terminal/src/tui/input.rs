//! Bounded byte framing before a library decoder sees terminal input.
#[cfg(unix)]
use std::time::Duration;
use termina::Event;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

pub(super) const MAX_TEXT: usize = rsi_agent_session_protocol::MAXIMUM_TURN_TEXT_BYTES;
const MAX_FRAME: usize = 4096;
const PASTE_END: &[u8] = b"\x1b[201~";

#[derive(Debug)]
pub(super) enum Input {
    Terminal(Event),
    Rejected(&'static str),
    Closed,
}

#[derive(Debug, Default)]
pub(super) struct Framer {
    frame: Vec<u8>,
    paste: Option<Vec<u8>>,
    paste_match: usize,
    overflow: bool,
    discard: Option<bool>, // true: OSC/DCS string, false: CSI
    discard_escape: bool,
}

impl Framer {
    pub(super) fn advance(&mut self, byte: u8) -> Option<Input> {
        if self.paste.is_some() {
            return self.paste_byte(byte);
        }
        if let Some(string) = self.discard {
            let ended = if string {
                byte == 7 || (self.discard_escape && byte == b'\\')
            } else {
                (0x40..=0x7e).contains(&byte)
            };
            self.discard_escape = byte == 27;
            if ended {
                self.discard = None;
            }
            return None;
        }
        self.frame.push(byte);
        let escape = self.frame[0] == 27;
        let string = escape && matches!(self.frame.get(1), Some(b']' | b'P' | b'_' | b'^'));
        let complete = if string {
            byte == 7 || self.frame.ends_with(b"\x1b\\")
        } else if escape && matches!(self.frame.get(1), Some(b'[' | b'O')) {
            self.frame.len() > 2 && (0x40..=0x7e).contains(&byte)
        } else if escape && self.frame.len() == 1 {
            false
        } else {
            let text = if escape {
                &self.frame[1..]
            } else {
                &self.frame[..]
            };
            match std::str::from_utf8(text) {
                Ok(_) => true,
                Err(error) if error.error_len().is_none() => false,
                Err(_) => {
                    self.frame.clear();
                    return Some(Input::Rejected("Input is not UTF-8"));
                }
            }
        };
        if self.frame.len() > MAX_FRAME {
            self.frame.clear();
            if !complete {
                self.discard = Some(string);
            }
            return Some(Input::Rejected("Terminal input sequence exceeds its limit"));
        }
        if !complete {
            return None;
        }
        if self.frame == b"\x1b[200~" {
            self.frame.clear();
            self.paste = Some(Vec::new());
            self.paste_match = 0;
            self.overflow = false;
            return None;
        }
        let mut parser = termina::Parser::default();
        parser.parse(&self.frame, false);
        self.frame.clear();
        parser.pop().map(Input::Terminal)
    }

    fn retain_paste_byte(&mut self, byte: u8) {
        let body = self.paste.as_mut().expect("paste is active");
        if self.overflow {
            return;
        }
        if body.len() == MAX_TEXT {
            *body = Vec::new();
            self.overflow = true;
        } else {
            body.push(byte);
        }
    }

    fn paste_byte(&mut self, byte: u8) -> Option<Input> {
        if byte == PASTE_END[self.paste_match] {
            self.paste_match += 1;
            if self.paste_match != PASTE_END.len() {
                return None;
            }
            let body = self.paste.take().expect("paste is active");
            self.paste_match = 0;
            if self.overflow {
                return Some(Input::Rejected("Paste exceeds 1 MiB; nothing was inserted"));
            }
            return Some(match String::from_utf8(body) {
                Ok(text) => Input::Terminal(Event::Paste(text)),
                Err(_) => Input::Rejected("Paste is not UTF-8; nothing was inserted"),
            });
        }
        let matched = self.paste_match;
        self.paste_match = 0;
        for pending in &PASTE_END[..matched] {
            self.retain_paste_byte(*pending);
        }
        if byte == PASTE_END[0] {
            self.paste_match = 1;
        } else {
            self.retain_paste_byte(byte);
        }
        None
    }

    pub(super) fn flush_escape(&mut self) -> Option<Input> {
        if self.frame == [27] {
            self.frame.clear();
            Some(Input::Terminal(Event::Key(
                termina::event::KeyCode::Escape.into(),
            )))
        } else if self.frame.first() == Some(&27) {
            self.frame.clear();
            Some(Input::Rejected(
                "Incomplete terminal escape sequence discarded",
            ))
        } else {
            None
        }
    }
}

#[cfg(unix)]
pub(super) fn spawn(
    stop: CancellationToken,
    tasks: &tokio_util::task::TaskTracker,
) -> std::io::Result<mpsc::Receiver<Input>> {
    use std::io::Read as _;
    use std::os::unix::fs::OpenOptionsExt as _;
    let tty = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open("/dev/tty")?;
    let tty = tokio::io::unix::AsyncFd::new(tty)?;
    let (sender, receiver) = mpsc::channel(8);
    tasks.spawn(async move {
        let mut framer = Framer::default();
        let mut bytes = [0; 4096];
        loop {
            let read_result = tokio::select! {
                () = stop.cancelled() => return,
                () = tokio::time::sleep(Duration::from_millis(50)) => {
                    if let Some(event) = framer.flush_escape() {
                        tokio::select! {
                            () = stop.cancelled() => return,
                            result = sender.send(event) => if result.is_err() { return; },
                        }
                    }
                    continue;
                },
                ready = tty.readable() => match ready {
                    Ok(mut ready) => match ready.try_io(|fd| fd.get_ref().read(&mut bytes)) {
                        Ok(result) => result,
                        Err(_) => continue,
                    },
                    Err(error) => Err(error),
                },
            };
            let Ok(count) = read_result else {
                break;
            };
            if count == 0 {
                break;
            }
            for byte in &bytes[..count] {
                if let Some(event) = framer.advance(*byte) {
                    tokio::select! {
                        () = stop.cancelled() => return,
                        result = sender.send(event) => if result.is_err() { return; },
                    }
                }
            }
        }
        tokio::select! {
            () = stop.cancelled() => {},
            _ = sender.send(Input::Closed) => {},
        }
    });
    Ok(receiver)
}

#[cfg(not(unix))]
pub(super) fn spawn(
    _: CancellationToken,
    _: &tokio_util::task::TaskTracker,
) -> std::io::Result<mpsc::Receiver<Input>> {
    Err(std::io::Error::other(
        "Fullscreen input currently requires a Unix terminal",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use termina::event::{KeyCode, Modifiers};

    fn feed(framer: &mut Framer, bytes: &[u8]) -> Vec<Input> {
        bytes
            .iter()
            .filter_map(|byte| framer.advance(*byte))
            .collect()
    }

    #[test]
    fn paste_then_enter_is_one_atomic_paste_followed_by_submit() {
        let events = feed(&mut Framer::default(), b"\x1b[200~one\ntwo\x1b[201~\r");
        assert!(matches!(&events[0], Input::Terminal(Event::Paste(text)) if text == "one\ntwo"));
        assert!(
            matches!(&events[1], Input::Terminal(Event::Key(key)) if key.code == KeyCode::Enter)
        );
        assert_eq!(events.len(), 2);
    }

    #[test]
    fn incomplete_escape_expires_without_consuming_following_keys_or_paste() {
        for prefix in [
            b"\x1b[".as_slice(),
            b"\x1b[12;",
            b"\x1b]title",
            b"\x1bPstring",
        ] {
            let mut framer = Framer::default();
            assert!(feed(&mut framer, prefix).is_empty());
            assert!(matches!(framer.flush_escape(), Some(Input::Rejected(_))));
            assert!(
                matches!(framer.advance(b'x'), Some(Input::Terminal(Event::Key(key))) if key.code == KeyCode::Char('x'))
            );
        }
        let mut framer = Framer::default();
        feed(&mut framer, b"\x1b[200~half");
        assert!(framer.flush_escape().is_none());
        let events = feed(&mut framer, b" paste\x1b[201~");
        assert!(matches!(&events[0], Input::Terminal(Event::Paste(text)) if text == "half paste"));
    }

    #[test]
    fn oversized_paste_discards_commands_through_split_terminator() {
        let mut framer = Framer::default();
        feed(&mut framer, b"\x1b[200~");
        for _ in 0..MAX_TEXT + 100 {
            assert!(framer.advance(b'a').is_none());
        }
        assert!(framer.paste.as_ref().unwrap().is_empty());
        assert!(feed(&mut framer, b"\r\x03\x1b[20").is_empty());
        let end = feed(&mut framer, b"1~x");
        assert!(matches!(end[0], Input::Rejected(_)));
        assert!(
            matches!(&end[1], Input::Terminal(Event::Key(key)) if key.code == KeyCode::Char('x'))
        );
    }

    #[test]
    fn unicode_and_legacy_and_enhanced_keys_survive_framing() {
        let events = feed(&mut Framer::default(), "界\r\n\u{f}\u{1b}[13;2u".as_bytes());
        let keys: Vec<_> = events
            .into_iter()
            .filter_map(|event| match event {
                Input::Terminal(Event::Key(key)) => Some(key),
                _ => None,
            })
            .collect();
        assert_eq!(keys[0].code, KeyCode::Char('界'));
        assert_eq!(keys[1].code, KeyCode::Enter);
        assert_eq!(keys[2].code, KeyCode::Char('j'));
        assert!(keys[2].modifiers.contains(Modifiers::CONTROL));
        assert_eq!(keys[3].code, KeyCode::Char('o'));
        assert!(keys[4].modifiers.contains(Modifiers::SHIFT));
    }

    #[test]
    fn unterminated_escape_and_paste_have_bounded_storage() {
        let mut framer = Framer::default();
        feed(&mut framer, b"\x1b]");
        for _ in 0..MAX_FRAME * 10 {
            framer.advance(b'x');
        }
        assert!(framer.frame.is_empty());
        assert_eq!(feed(&mut framer, b"\x07a").len(), 1);
        feed(&mut framer, b"\x1b[200~");
        for _ in 0..MAX_TEXT * 2 {
            framer.advance(b'x');
        }
        assert!(framer.paste.as_ref().unwrap().is_empty());
    }
}
