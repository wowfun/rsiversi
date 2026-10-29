use pulldown_cmark::{
    Alignment as ParsedAlignment, CodeBlockKind, Event, Options, Parser, Tag, TagEnd,
};
use serde::Serialize;

const MAXIMUM_INPUT_BYTES: usize = 64 * 1024;
const MAXIMUM_EVENTS: usize = 4096;
const MAXIMUM_DEPTH: usize = 32;
const FIXED_OVERHEAD_BYTES: usize = 16 * 1024;

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
    Pre { language: Option<String> },
    Table,
    TableHead,
    TableRow,
    TableCell { alignment: Alignment, header: bool },
    List { start: Option<u32> },
    Item,
    Emphasis,
    Strong,
    Strike,
    Link { href: String },
    Span,
}
#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Alignment {
    None,
    Left,
    Center,
    Right,
}
fn language(info: &str) -> Option<String> {
    let token = info.split_whitespace().next()?;
    (token.len() <= 32
        && token
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"_+-.#".contains(&byte)))
    .then(|| token.to_ascii_lowercase())
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
    let mut columns = Vec::new();
    let mut column = 0;
    let mut header = false;
    for event in Parser::new_ext(
        source,
        Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TABLES,
    ) {
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
                    Tag::CodeBlock(kind) => Element::Pre {
                        language: match kind {
                            CodeBlockKind::Fenced(info) => language(&info),
                            CodeBlockKind::Indented => None,
                        },
                    },
                    Tag::Table(alignments) => {
                        columns = alignments;
                        Element::Table
                    }
                    Tag::TableHead => {
                        column = 0;
                        header = true;
                        Element::TableHead
                    }
                    Tag::TableRow => {
                        column = 0;
                        Element::TableRow
                    }
                    Tag::TableCell => {
                        let alignment = match columns.get(column)? {
                            ParsedAlignment::None => Alignment::None,
                            ParsedAlignment::Left => Alignment::Left,
                            ParsedAlignment::Center => Alignment::Center,
                            ParsedAlignment::Right => Alignment::Right,
                        };
                        column += 1;
                        Element::TableCell { alignment, header }
                    }
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
            Event::End(tag) => {
                if tag == TagEnd::TableHead {
                    header = false;
                }
                if tag == TagEnd::Table {
                    columns.clear();
                }
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
    if depth != 0 {
        return None;
    }
    rsi_api_protocol::measure_json(&nodes, source.len() * 4 + FIXED_OVERHEAD_BYTES).ok()?;
    (retained_bytes(&nodes) <= source.len() * 8 + FIXED_OVERHEAD_BYTES).then_some(nodes)
}

// Bound retained cache capacity independently of its encoded frame expansion.
fn retained_bytes(nodes: &Vec<Node>) -> usize {
    nodes.capacity() * std::mem::size_of::<Node>()
        + nodes
            .iter()
            .map(|node| match node {
                Node::Text { text } | Node::Code { text } => text.capacity(),
                Node::Start {
                    element: Element::Link { href },
                } => href.capacity(),
                Node::Start {
                    element:
                        Element::Pre {
                            language: Some(language),
                        },
                } => language.capacity(),
                _ => 0,
            })
            .sum::<usize>()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn tables_preserve_alignment_headers_and_literal_cell_content() {
        let source = "| Left column with enough source text | Center column with enough source text | Right column with enough source text |\n| :--- | :---: | ---: |\n| first value with **emphasis** | second literal `<script>` value | third value |\n| fourth extended value | fifth extended value | sixth extended value |";
        let source = format!(
            "{}\n\n{source}",
            "This table preserves header cells, alignment and exact literal values. ".repeat(8)
        );
        let nodes = parse(&source).unwrap();
        let cells = nodes
            .iter()
            .filter_map(|node| match node {
                Node::Start {
                    element: Element::TableCell { alignment, header },
                } => Some((alignment, *header)),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(cells.len(), 9);
        assert!(matches!(cells[0], (Alignment::Left, true)));
        assert!(matches!(cells[1], (Alignment::Center, true)));
        assert!(matches!(cells[2], (Alignment::Right, true)));
        assert!(cells[3..].iter().all(|(_, header)| !header));
        assert!(
            nodes
                .iter()
                .any(|node| matches!(node, Node::Code { text } if text == "<script>"))
        );
        assert!(parse("|a|b|\n|-|-|\n|c|d|").is_some());
    }
    #[test]
    fn ordinary_short_table_and_code_survive_presentation_budgets() {
        // A real provider response previously fell back to raw Markdown because
        // its structural overhead dominated its short cell and code contents.
        let source = "| Item | Value |\n| --- | --- |\n| alpha | 7 |\n| beta | 11 |\n\n```js\nconst sum = 7 + 11;\n```\nRSI_MARKDOWN_OK_1027";
        let nodes = parse(source).unwrap();
        assert!(nodes.iter().any(|node| matches!(
            node,
            Node::Start {
                element: Element::Table
            }
        )));
        assert!(nodes.iter().any(|node| matches!(node, Node::Start { element: Element::Pre { language: Some(language) } } if language == "js")));
    }
    #[test]
    fn ordinary_ten_row_four_column_table_remains_structured() {
        let source = format!(
            "| A | B | C | D |\n|---|---|---|---|\n{}",
            "| 1 | 2 | 3 | 4 |\n".repeat(10)
        );
        let nodes = parse(&source).expect("ordinary table must remain structured");
        assert_eq!(
            nodes
                .iter()
                .filter(|node| matches!(
                    node,
                    Node::Start {
                        element: Element::TableCell { .. }
                    }
                ))
                .count(),
            44
        );
    }
    #[test]
    fn fenced_language_is_bounded_metadata_not_markup() {
        assert_eq!(language("JS title=example"), Some("js".into()));
        assert_eq!(language("c++"), Some("c++".into()));
        for value in ["", "界", "<script>", "a\"onclick=x", &"x".repeat(33)] {
            assert_eq!(language(value), None);
        }
        let nodes =
            parse("```JS title=example\nconsole.log('bounded code example');\n```\n").unwrap();
        assert!(nodes.iter().any(|node| matches!(node, Node::Start { element: Element::Pre { language: Some(value) } } if value == "js")));
        assert!(
            parse(&format!(
                "```{}\nplain code remains readable\n```",
                "x".repeat(33)
            ))
            .is_some()
        );
    }
    #[test]
    fn encoded_budget_matches_serde_for_escaped_and_multibyte_nodes() {
        for text in [
            "\"\\\n\r\t\0",
            "界🦀e\u{301}\u{2028}\u{2029}",
            "<script>& /",
            "",
        ] {
            let nodes = vec![
                Node::Text { text: text.into() },
                Node::Code { text: text.into() },
                Node::Start {
                    element: Element::Link { href: text.into() },
                },
                Node::End,
            ];
            let length = serde_json::to_vec(&nodes).unwrap().len();
            assert_eq!(
                rsi_api_protocol::measure_json(&nodes, length).unwrap(),
                length
            );
            assert!(rsi_api_protocol::measure_json(&nodes, length - 1).is_err());
        }
    }
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
            parse(&"*x* ".repeat(500)).is_none(),
            "encoded overhead is independently bounded"
        );
    }
}
