//! Text retention with an explicit token budget and recoverable output locator.
#![deny(unsafe_code)]
#![warn(missing_docs)]

use rsi_spill::SpillRef;
use serde::{Deserialize, Serialize};

/// Optional retention budget; omission disables policy registration.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SpillPolicyConfig {
    /// Maximum estimated inline tokens, including framing and retrieval notice.
    #[serde(default)]
    pub max_inline_tokens: Option<usize>,
}

/// Estimates a single text block without pricing unsupported image content.
fn estimated_text_tokens(text: &str) -> usize {
    4 + text.chars().count().div_ceil(4)
}

use rsi_spill::retrieval_notice;

/// Selects an ordered UTF-8 head/tail within the budget, or declines replacement.
/// Callers have already saved the complete text; the canonical value is unrelated.
pub fn retain_text(text: &str, reference: &SpillRef, maximum_tokens: usize) -> Option<String> {
    let notice = retrieval_notice(reference);
    let preview = retain_preview(text, &notice, maximum_tokens)?;
    Some(if preview.is_empty() {
        notice
    } else {
        format!("{preview}\n\n{notice}")
    })
}

fn retain_preview(text: &str, notice: &str, maximum_tokens: usize) -> Option<String> {
    const GAP: &str = "\n\n[... omitted ...]\n\n";
    if estimated_text_tokens(notice) > maximum_tokens {
        return None;
    }
    let available = maximum_tokens.saturating_sub(4).saturating_mul(4);
    let framing = GAP.chars().count() + 2 + notice.chars().count();
    if available <= framing {
        return Some(String::new());
    }
    let selected = text.chars().take(available - framing).count();
    let head = selected.div_ceil(2);
    let tail = selected / 2;
    let head_end = text
        .char_indices()
        .nth(head)
        .map_or(text.len(), |(at, _)| at);
    let tail_start = if tail == 0 {
        text.len()
    } else {
        text.char_indices()
            .rev()
            .nth(tail - 1)
            .map_or(0, |(at, _)| at)
    };
    if head_end >= tail_start {
        Some(text.to_owned())
    } else {
        Some(format!("{}{GAP}{}", &text[..head_end], &text[tail_start..]))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rsi_spill::{SpillLocator, SpillScope};
    #[test]
    fn a_preview_that_contains_every_character_has_no_omission_marker() {
        assert_eq!(
            retain_preview("complete", "notice", 100).unwrap(),
            "complete"
        );
    }
    #[test]
    fn unicode_budget_preserves_order_and_the_complete_locator() {
        let text = format!("HEAD{}TAIL", "中🦀".repeat(10000));
        let reference = SpillRef {
            locator: SpillLocator::new(&SpillScope::new("session", "fixture"), [0xab; 16]),
            bytes: text.len() as u64,
        };
        let notice = retrieval_notice(&reference);
        assert!(retain_text(&text, &reference, estimated_text_tokens(&notice) - 1).is_none());
        for budget in [estimated_text_tokens(&notice), 100, 250, 2000] {
            let retained = retain_text(&text, &reference, budget).unwrap();
            assert!(estimated_text_tokens(&retained) <= budget);
            assert!(retained.ends_with(&notice));
            if budget >= 100 {
                assert!(retained.starts_with("HEAD"));
                assert!(retained.contains("TAIL\n\n"));
            }
        }
        assert_eq!(estimated_text_tokens("中🦀ab"), 5);
        assert_eq!(
            serde_json::from_str::<SpillPolicyConfig>("{}")
                .unwrap()
                .max_inline_tokens,
            None
        );
    }
}

mod plugin;
pub use plugin::SpillPolicyFactory;
