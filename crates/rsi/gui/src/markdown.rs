use pulldown_cmark::{Event, Options, Parser, Tag};
use serde::Serialize;

const MAXIMUM_INPUT_BYTES: usize = 64 * 1024;
const MAXIMUM_EVENTS: usize = 4096;
const MAXIMUM_DEPTH: usize = 32;

#[derive(Clone, Debug, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum Node {
    Start { element: Element },
    End,
    Text { text: String },
    Code { text: String },
    Break,
    Rule,
}
#[derive(Clone, Debug, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum Element {
    Paragraph,
    Heading { level: u8 },
    Quote,
    Pre,
    List { start: Option<u32> },
    Item,
    Emphasis,
    Strong,
    Strike,
    Link { href: String },
    Span,
}
fn link(value: &str) -> Option<String> {
    if value.len() > 2048 {
        return None;
    }
    let parsed = url::Url::parse(value).ok()?;
    matches!(parsed.scheme(), "http" | "https").then(|| parsed.into())
}
/// Failure preserves plain source, including unsupported or over-budget input.
pub(crate) fn parse(source: &str) -> Option<Vec<Node>> {
    if source.len() > MAXIMUM_INPUT_BYTES {
        return None;
    }
    let mut nodes = Vec::new();
    let mut depth = 0usize;
    for event in Parser::new_ext(source, Options::ENABLE_STRIKETHROUGH) {
        if nodes.len() == MAXIMUM_EVENTS {
            return None;
        }
        let node = match event {
            Event::Start(tag) => {
                depth += 1;
                if depth > MAXIMUM_DEPTH {
                    return None;
                }
                let element = match tag {
                    Tag::Paragraph => Element::Paragraph,
                    Tag::Heading { level, .. } => Element::Heading { level: level as u8 },
                    Tag::BlockQuote => Element::Quote,
                    Tag::CodeBlock(_) => Element::Pre,
                    Tag::HtmlBlock | Tag::Image { .. } => Element::Span,
                    Tag::List(start) => Element::List {
                        start: start.map(u32::try_from).transpose().ok()?,
                    },
                    Tag::Item => Element::Item,
                    Tag::Emphasis => Element::Emphasis,
                    Tag::Strong => Element::Strong,
                    Tag::Strikethrough => Element::Strike,
                    Tag::Link { dest_url, .. } => {
                        link(&dest_url).map_or(Element::Span, |href| Element::Link { href })
                    }
                    _ => return None,
                };
                Node::Start { element }
            }
            Event::End(_) => {
                depth = depth.checked_sub(1)?;
                Node::End
            }
            Event::Text(text) | Event::Html(text) | Event::InlineHtml(text) => Node::Text {
                text: text.into_string(),
            },
            Event::Code(text) => Node::Code {
                text: text.into_string(),
            },
            Event::SoftBreak => Node::Text { text: "\n".into() },
            Event::HardBreak => Node::Break,
            Event::Rule => Node::Rule,
            _ => return None,
        };
        nodes.push(node);
    }
    if depth != 0 || serde_json::to_vec(&nodes).ok()?.len() > source.len() * 4 + 256 {
        return None;
    }
    // Bound retained cache capacity independently of its encoded frame expansion.
    let owned = nodes.capacity() * std::mem::size_of::<Node>()
        + nodes
            .iter()
            .map(|node| match node {
                Node::Text { text } | Node::Code { text } => text.capacity(),
                Node::Start {
                    element: Element::Link { href },
                } => href.capacity(),
                _ => 0,
            })
            .sum::<usize>();
    (owned <= source.len() * 8 + 512).then_some(nodes)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn closed_markdown_preserves_literals_and_restricts_navigation() {
        let source = "## Result\n\n**bold 界** and `literal <x>` with [site](https://example.com/a).\n\n<script>bad()</script>\n\n![remote](https://example.com/image.png)\n\n[bad](javascript:alert%281%29)\n\n```sh\nexit 7\n```";
        let nodes = parse(source).unwrap();
        let json = serde_json::to_value(&nodes).unwrap().to_string();
        assert!(json.contains("heading"));
        assert!(json.contains("strong"));
        assert!(json.contains("literal <x>"));
        assert!(json.contains("<script>bad()</script>"));
        assert!(!json.contains("javascript:"));
        assert!(!json.contains("image.png"));
        assert!(json.contains("https://example.com/a"));
        for invalid in [
            "javascript:alert(1)",
            "data:text/html,x",
            "file:///private",
            "/relative",
            "//remote/x",
        ] {
            assert!(link(invalid).is_none());
        }
    }
    #[test]
    fn source_event_depth_and_encoded_expansion_bounds_fall_back_to_plain_text() {
        assert!(parse(&"x".repeat(MAXIMUM_INPUT_BYTES)).is_some());
        assert!(parse(&"x".repeat(MAXIMUM_INPUT_BYTES + 1)).is_none());
        assert!(parse(&format!("{}deep", "> ".repeat(MAXIMUM_DEPTH + 1))).is_none());
        assert!(parse(&"*x* ".repeat(MAXIMUM_EVENTS)).is_none());
        assert!(
            parse(&"*x* ".repeat(100)).is_none(),
            "encoded overhead is independently bounded"
        );
    }
}
