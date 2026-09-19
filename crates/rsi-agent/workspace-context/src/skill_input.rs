//! Pure skill-reference syntax shared by invocation and editor completion.
use pulldown_cmark::{Event, LinkType, Parser, Tag, TagEnd};
use std::ops::Range;

/// A dollar token in prose. An empty name is useful while completing `$`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SkillToken<'a> {
    /// Original UTF-8 byte range, including the dollar sign.
    pub range: Range<usize>,
    /// Exact name, without the sigil.
    pub name: &'a str,
}

fn name_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.')
}

/// Borrows prose tokens without loading skills or granting invocation authority.
/// Code, escaped sigils, HTML and link destinations are excluded.
pub fn dollar_tokens(text: &str) -> impl Iterator<Item = SkillToken<'_>> {
    let mut code = false;
    let mut auto_link = false;
    Parser::new(text)
        .into_offset_iter()
        .filter_map(move |(event, range)| {
            match event {
                Event::Start(Tag::CodeBlock(_)) => code = true,
                Event::End(TagEnd::CodeBlock) => code = false,
                Event::Start(Tag::Link {
                    link_type: LinkType::Autolink | LinkType::Email,
                    ..
                }) => auto_link = true,
                Event::End(TagEnd::Link) => auto_link = false,
                Event::Text(_) if !code && !auto_link => return Some(range),
                _ => {}
            }
            None
        })
        .flat_map(move |range| {
            text[range.clone()]
                .match_indices('$')
                .filter_map(move |(offset, _)| {
                    let start = range.start + offset;
                    let bytes = text.as_bytes();
                    let escapes = bytes[..start]
                        .iter()
                        .rev()
                        .take_while(|b| **b == b'\\')
                        .count();
                    if escapes % 2 == 1
                        || start > 0 && (name_byte(bytes[start - 1]) || bytes[start - 1] == b'$')
                    {
                        return None;
                    }
                    let end = start
                        + 1
                        + bytes[start + 1..]
                            .iter()
                            .take_while(|b| name_byte(**b))
                            .count();
                    let name = &text[start + 1..end];
                    (name.is_empty() || super::valid_skill_name(name)).then_some(SkillToken {
                        range: start..end,
                        name,
                    })
                })
        })
}

/// Returns only the eligible token containing the cursor, including its suffix.
pub fn dollar_token_at(text: &str, cursor: usize) -> Option<SkillToken<'_>> {
    if !text.is_char_boundary(cursor) {
        return None;
    }
    dollar_tokens(text).find(|token| token.range.start < cursor && cursor <= token.range.end)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prose_only_with_exact_original_ranges() {
        let input = "请用 $review 和 **$build** [$link](https://host/$hidden) ` $inline ` \\$escaped\n```sh\n$fenced\n```\n    $indented\n\n$last";
        let tokens: Vec<_> = dollar_tokens(input).collect();
        assert_eq!(
            tokens.iter().map(|t| t.name).collect::<Vec<_>>(),
            ["review", "build", "link", "last"]
        );
        for token in tokens {
            assert_eq!(&input[token.range], format!("${}", token.name));
        }
    }

    #[test]
    fn completion_keeps_suffix_and_rejects_code_and_escapes() {
        assert_eq!(
            dollar_token_at("中文 $review end", 10).unwrap().range,
            7..14
        );
        assert_eq!(dollar_token_at("text $", 6).unwrap().name, "");
        assert!(dollar_token_at("`$review`", 5).is_none());
        assert!(dollar_token_at("\\$review", 5).is_none());
        assert!(dollar_token_at("$$review", 5).is_none());
        assert!(dollar_token_at("${review}", 5).is_none());
    }
}
