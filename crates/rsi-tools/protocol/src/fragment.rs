use crate::{Result, ToolContent, ToolError, ToolResult};
use serde_json::Value;

/// Selects a UTF-8 fragment under a 1024..=16384 byte rendered-page budget.
///
/// `envelope(end)` wraps `source[offset..end]` with the caller's metadata. The
/// caller authenticates its cursor and supplies a deterministic envelope whose
/// encoded size grows with the fragment (apart from final-page cursor fields).
/// The budget includes the label, JSON escaping and all envelope metadata.
/// Rejects non-boundary cursors, oversized metadata and nonterminal empty pages.
pub fn bounded_json_fragment_page(
    source: &str,
    offset: usize,
    maximum: usize,
    label: &str,
    envelope: impl Fn(usize) -> Value,
) -> Result<ToolResult> {
    let invalid = |message: &str| ToolError::InvalidInput(message.into());
    let end = bounded_json_fragment_end(source, offset, maximum, label, &envelope)?;
    let value = envelope(end);
    let text = format!(
        "{label}{}",
        serde_json::to_string(&value).map_err(|_| invalid("invalid result page"))?
    );
    if text.len() > maximum {
        return Err(invalid("result metadata exceeds presentation budget"));
    }
    ToolResult::new(value, vec![ToolContent::Text { text }], false)
}

/// Chooses a UTF-8 end cursor by measuring a serializable envelope without a payload buffer.
/// Uses the same rendered-page bounds as [`bounded_json_fragment_page`].
/// Serialization failures are errors; only an exhausted byte counter means that
/// a candidate does not fit.
pub fn bounded_json_fragment_end<T: serde::Serialize>(
    source: &str,
    offset: usize,
    maximum: usize,
    label: &str,
    envelope: impl Fn(usize) -> T,
) -> Result<usize> {
    let invalid = |message: &str| ToolError::InvalidInput(message.into());
    if !(1024..=16384).contains(&maximum) || !source.is_char_boundary(offset) {
        return Err(invalid(
            "result page maximum or UTF-8 cursor is out of bounds",
        ));
    }
    let boundaries = source[offset..]
        .char_indices()
        .map(|(relative, ch)| offset + relative + ch.len_utf8())
        .take(maximum)
        .collect::<Vec<_>>();
    let fits = |end| -> Result<bool> {
        if label.len() > maximum {
            return Ok(false);
        }
        let mut counter = FragmentCounter {
            remaining: maximum - label.len(),
            exceeded: false,
        };
        match serde_json::to_writer(&mut counter, &envelope(end)) {
            Ok(()) => Ok(true),
            Err(error) if counter.exceeded && error.is_io() => Ok(false),
            Err(error) => Err(invalid(&format!("invalid result page: {error}"))),
        }
    };
    let (mut low, mut high) = (0, boundaries.len());
    // Final cursor fields can be shorter than the nonterminal form.
    if boundaries.last() == Some(&source.len()) && fits(source.len())? {
        low = boundaries.len();
    } else {
        while low < high {
            let middle = low + (high - low).div_ceil(2);
            if fits(boundaries[middle - 1])? {
                low = middle;
            } else {
                high = middle - 1;
            }
        }
    }
    let end = if low == 0 {
        offset
    } else {
        boundaries[low - 1]
    };
    if end == offset && end != source.len() {
        return Err(invalid(
            "result page budget cannot contain its metadata and one character",
        ));
    }
    if !fits(end)? {
        return Err(invalid("result metadata exceeds presentation budget"));
    }
    Ok(end)
}

struct FragmentCounter {
    remaining: usize,
    exceeded: bool,
}
impl std::io::Write for FragmentCounter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.remaining = self.remaining.checked_sub(bytes.len()).ok_or_else(|| {
            self.exceeded = true;
            std::io::Error::other("fragment exceeds page budget")
        })?;
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn serializable_envelope_failures_are_not_shorter_pages() {
        struct Envelope(usize);
        impl serde::Serialize for Envelope {
            fn serialize<S: serde::Serializer>(
                &self,
                serializer: S,
            ) -> std::result::Result<S::Ok, S::Error> {
                if self.0 >= 4 {
                    return Err(serde::ser::Error::custom("fixture serialization failed"));
                }
                serializer.serialize_str("small")
            }
        }
        assert!(bounded_json_fragment_end("abcdef", 0, 1024, "", Envelope).is_err());
    }
    #[test]
    fn serialization_failure_after_counter_exhaustion_is_not_a_shorter_page() {
        struct Envelope(usize);
        impl serde::Serialize for Envelope {
            fn serialize<S: serde::Serializer>(
                &self,
                serializer: S,
            ) -> std::result::Result<S::Ok, S::Error> {
                use serde::ser::SerializeTuple;
                if self.0 == 0 {
                    return serializer.serialize_str("small");
                }
                let mut tuple = serializer.serialize_tuple(2)?;
                let _exhausted = tuple.serialize_element(&"x".repeat(2048));
                Err(serde::ser::Error::custom(
                    "fixture serialization failed after exhaustion",
                ))
            }
        }
        let error = bounded_json_fragment_end("x", 0, 1024, "", Envelope).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("serialization failed after exhaustion")
        );
    }
    #[test]
    fn impossible_metadata_invalid_cursor_and_empty_complete_pages_are_distinct() {
        assert!(bounded_json_fragment_page("中", 1, 1024, "", |_| json!({})).is_err());
        assert!(bounded_json_fragment_page("x", 2, 1024, "", |_| json!({})).is_err());
        assert!(bounded_json_fragment_page("x", 0, 1023, "", |_| json!({})).is_err());
        assert!(bounded_json_fragment_page("x", 0, 16385, "", |_| json!({})).is_err());
        assert!(
            bounded_json_fragment_page("x", 0, 1024, &"m".repeat(1024), |_| json!({})).is_err()
        );
        assert!(bounded_json_fragment_page("", 0, 1024, &"m".repeat(1024), |_| json!({})).is_err());
        assert_eq!(
            bounded_json_fragment_page("", 0, 1024, "data", |_| json!({"fragment":""}))
                .unwrap()
                .value,
            json!({"fragment":""})
        );
    }
    #[test]
    fn escaped_multibyte_fragment_makes_progress_within_full_envelope_budget() {
        let source = "中\n\"\\".repeat(1000);
        let mut offset = 0;
        let mut restored = String::new();
        while offset < source.len() {
            let page = bounded_json_fragment_page(&source, offset, 1024, "Data:\n", |end| json!({"offset":offset,"next_offset":(end<source.len()).then_some(end),"fragment":&source[offset..end]})).unwrap();
            assert!(matches!(&page.content[0], ToolContent::Text { text } if text.len() <= 1024));
            let fragment = page.value["fragment"].as_str().unwrap();
            assert!(!fragment.is_empty());
            restored.push_str(fragment);
            offset += fragment.len();
        }
        assert_eq!(restored, source);
    }
}
