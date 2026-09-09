use crate::MAXIMUM_WINDOW_BYTES;
use std::fmt::Write;

/// Formats exact bytes with absolute offsets, without interpreting control bytes.
/// Returns `None` for an oversized window or an overflowing source range.
pub fn hex_window(bytes: &[u8], offset: u64) -> Option<String> {
    if bytes.len() > MAXIMUM_WINDOW_BYTES {
        return None;
    }
    offset.checked_add(bytes.len() as u64)?;
    let mut text = String::new();
    for (index, line) in bytes.chunks(16).enumerate() {
        let _ = write!(text, "{:08x}  ", offset + index as u64 * 16);
        for byte in line {
            let _ = write!(text, "{byte:02x} ");
        }
        text.push('\n');
    }
    Some(text)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn exact_offsets_and_uninterpreted_bytes_have_bounded_ranges() {
        assert_eq!(
            hex_window(&[0, 255, 27], 4096).unwrap(),
            "00001000  00 ff 1b \n"
        );
        let text = hex_window(&[0; 17], 4096).unwrap();
        assert!(text.lines().nth(1).unwrap().starts_with("00001010  00"));
        assert!(hex_window(&[0], u64::MAX).is_none());
        assert!(hex_window(&[], u64::MAX).unwrap().is_empty());
        assert!(hex_window(&vec![0; MAXIMUM_WINDOW_BYTES + 1], 0).is_none());
    }
}
