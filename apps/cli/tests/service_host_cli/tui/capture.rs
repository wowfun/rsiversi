use std::collections::VecDeque;

const HALF: usize = 8 * 1024 * 1024;

#[derive(Default)]
pub(super) struct RawCapture {
    pub(super) prefix: Vec<u8>,
    pub(super) tail: VecDeque<u8>,
    pub(super) discarded: usize,
}

impl RawCapture {
    pub(super) fn push(&mut self, bytes: &[u8]) {
        let prefix = bytes.len().min(HALF - self.prefix.len());
        self.prefix.extend_from_slice(&bytes[..prefix]);
        for &byte in &bytes[prefix..] {
            if self.tail.len() == HALF {
                self.tail.pop_front();
                self.discarded += 1;
            }
            self.tail.push_back(byte);
        }
    }

    pub(super) fn complete(&self) -> Vec<u8> {
        assert_eq!(
            self.discarded, 0,
            "full-transcript assertion requires untruncated evidence"
        );
        self.prefix.iter().chain(&self.tail).copied().collect()
    }

    pub(super) fn assert_absent(&self, needle: &[u8]) {
        assert_eq!(
            self.discarded, 0,
            "absence assertions require complete evidence"
        );
        assert!(!self.contains(needle), "unexpected bytes in PTY capture");
    }

    pub(super) fn contains(&self, needle: &[u8]) -> bool {
        let (front, back) = self.tail.as_slices();
        if self.discarded == 0 {
            contains_slices(&[&self.prefix, front, back], needle)
        } else {
            contains_slices(&[&self.prefix], needle) || contains_slices(&[front, back], needle)
        }
    }
}

fn contains_slices(parts: &[&[u8]], needle: &[u8]) -> bool {
    if needle.is_empty() {
        return true;
    }
    parts.iter().enumerate().any(|(index, part)| {
        let remaining: usize = parts[index..].iter().map(|part| part.len()).sum();
        let Some(last_start) = remaining.checked_sub(needle.len()) else {
            return false;
        };
        let end = part.len().min(last_start + 1);
        part.windows(needle.len()).any(|bytes| bytes == needle)
            || (part.len().saturating_sub(needle.len() - 1)..end).any(|start| {
                part[start..]
                    .iter()
                    .chain(parts[index + 1..].iter().flat_map(|part| part.iter()))
                    .take(needle.len())
                    .eq(needle.iter())
            })
    })
}

#[test]
fn searches_match_contiguous_bytes_at_every_prefix_and_ring_boundary() {
    let bytes = b"ab\0cdabcd";
    for split in 0..=bytes.len() {
        let suffix = &bytes[split..];
        for rotation in 0..suffix.len().max(1) {
            let mut tail = VecDeque::from(suffix.to_vec());
            for _ in 0..rotation {
                let first = tail.pop_front().unwrap();
                tail.push_back(first);
            }
            for (cell, byte) in tail.iter_mut().zip(suffix) {
                *cell = *byte;
            }
            let mut raw = RawCapture {
                prefix: bytes[..split].to_vec(),
                tail,
                discarded: 0,
            };
            for discarded in [0, 1] {
                raw.discarded = discarded;
                for start in 0..=bytes.len() {
                    for end in start..=bytes.len() {
                        let needle = &bytes[start..end];
                        let contains = |part: &[u8]| {
                            needle.is_empty() || part.windows(needle.len()).any(|w| w == needle)
                        };
                        let expected = if discarded == 0 {
                            contains(bytes)
                        } else {
                            contains(&bytes[..split]) || contains(suffix)
                        };
                        assert_eq!(raw.contains(needle), expected);
                    }
                }
                assert!(!raw.contains(b"unavailable"));
                assert!(!raw.contains(b"ab\0cdabcde"));
            }
        }
    }
}

#[test]
fn bounded_raw_capture_does_not_stop_parsing_or_match_across_a_gap() {
    let mut raw = RawCapture::default();
    let mut parser = vt100::Parser::new(2, 40, 0);
    let chunks = [
        vec![b'a'; HALF],
        vec![b'b'; HALF],
        vec![b'c'; HALF],
        b"\x1b[2J\x1b[Hnew screen".to_vec(),
    ];
    for chunk in chunks {
        raw.push(&chunk);
        parser.process(&chunk);
    }
    assert_eq!(raw.prefix.len() + raw.tail.len(), 2 * HALF);
    assert!(raw.discarded > 0);
    assert!(parser.screen().contents().contains("new screen"));
    assert!(raw.contains(b"new screen"));
    assert!(!raw.contains(b"ac"));
    assert!(std::panic::catch_unwind(|| raw.complete()).is_err());
    assert!(std::panic::catch_unwind(|| raw.assert_absent(b"missing secret")).is_err());
}
