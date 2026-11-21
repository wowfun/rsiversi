use pulldown_cmark::{
    Alignment as ParsedAlignment, CodeBlockKind, Event, Options, Parser, Tag, TagEnd,
    TextMergeWithOffset,
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
    Math { text: String, display: bool },
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
    Image { src: String },
    FileImage { path: String },
    FileImageLink { path: String },
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

fn file_image(value: &str) -> Option<String> {
    let raw = value.split('#').next()?;
    if raw.strip_prefix(r"\\?\").unwrap_or(raw).contains('?')
        || raw.as_bytes().iter().enumerate().any(|(i, b)| {
            *b == b'%'
                && !raw
                    .as_bytes()
                    .get(i + 1..i + 3)
                    .is_some_and(|s| s.iter().all(u8::is_ascii_hexdigit))
        })
    {
        return None;
    }
    let path = percent_encoding::percent_decode_str(raw)
        .decode_utf8()
        .ok()?;
    if path.is_empty()
        || !rsi_session_files::is_local_image_path(path.as_bytes())
        || path.len() > rsi_files_protocol::MAXIMUM_FILE_PATH_BYTES
        || path.chars().any(char::is_control)
    {
        return None;
    }
    let bytes = path.as_bytes();
    let drive = bytes.len() >= 3
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && matches!(bytes[2], b'/' | b'\\');
    if !drive && url::Url::parse(&path).is_ok() {
        return None;
    }
    Some(path.into_owned())
}

fn image_link(value: &str) -> Option<String> {
    let path = file_image(value)?;
    has_image_extension(&path).then_some(path)
}

fn has_image_extension(path: &str) -> bool {
    path.rsplit_once('.').is_some_and(|(_, extension)| {
        ["png", "jpg", "jpeg", "gif", "webp", "svg", "bmp", "ico"]
            .iter()
            .any(|suffix| extension.eq_ignore_ascii_case(suffix))
    })
}

// DSH recovers an image only when the paragraph's complete literal text
// matches its source. Link destinations in ordinary Markdown stay with the parser.
fn spaced_image(source: &str) -> Option<(&str, String)> {
    let (alt, path) = source
        .strip_prefix("![")?
        .strip_suffix(')')?
        .split_once("](")?;
    if alt.contains([']', '\n', '\r'])
        || !path.contains(' ')
        || !has_image_extension(path)
        || path.contains(['\n', '\r', '<', '>', '(', ')', '[', ']', '"', '\''])
        || !(path.starts_with('/')
            || path.starts_with("./")
            || path.starts_with("../")
            || matches!(path.as_bytes(), [letter, b':', b'/' | b'\\', ..] if letter.is_ascii_alphabetic()))
    {
        return None;
    }
    Some((alt, file_image(path)?))
}

fn leaf(event: Event<'_>) -> Option<Node> {
    Some(match event {
        Event::Text(text) | Event::Html(text) | Event::InlineHtml(text) => Node::Text {
            text: text.into_string(),
        },
        Event::Code(text) => Node::Code {
            text: text.into_string(),
        },
        Event::InlineMath(text) => Node::Math {
            text: text.into_string(),
            display: false,
        },
        Event::DisplayMath(text) => Node::Math {
            text: text.into_string(),
            display: true,
        },
        Event::SoftBreak => Node::Text { text: "\n".into() },
        Event::HardBreak => Node::Break,
        Event::Rule => Node::Rule,
        _ => return None,
    })
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
    let parser = Parser::new_ext(
        source,
        Options::ENABLE_STRIKETHROUGH
            | Options::ENABLE_TABLES
            | Options::ENABLE_MATH
            | Options::ENABLE_CJK_STRONG
            | Options::PRESERVE_WINDOWS_PATHS,
    );
    let mut events = TextMergeWithOffset::new(parser.into_offset_iter()).peekable();
    while let Some((event, range)) = events.next() {
        if nodes.len() >= MAXIMUM_EVENTS {
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
                    Tag::BlockQuote(_) => Element::Quote,
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
                    Tag::HtmlBlock => Element::Span,
                    Tag::Image { dest_url, .. } => link(&dest_url)
                        .map(|src| Element::Image { src })
                        .or_else(|| file_image(&dest_url).map(|path| Element::FileImage { path }))
                        .unwrap_or(Element::Span),
                    Tag::List(start) => Element::List {
                        start: start.map(u32::try_from).transpose().ok()?,
                    },
                    Tag::Item => Element::Item,
                    Tag::Emphasis => Element::Emphasis,
                    Tag::Strong => Element::Strong,
                    Tag::Strikethrough => Element::Strike,
                    Tag::Link { dest_url, .. } => link(&dest_url)
                        .map(|href| Element::Link { href })
                        .or_else(|| {
                            image_link(&dest_url).map(|path| Element::FileImageLink { path })
                        })
                        .unwrap_or(Element::Span),
                    _ => return None,
                };
                Node::Start { element }
            }
            Event::End(tag) => {
                if tag == TagEnd::CodeBlock
                    && let Some(Node::Text { text }) = nodes.last_mut()
                    && text.ends_with('\n')
                {
                    text.pop();
                }
                if tag == TagEnd::TableHead {
                    header = false;
                }
                if tag == TagEnd::Table {
                    columns.clear();
                }
                depth = depth.checked_sub(1)?;
                Node::End
            }
            Event::Text(ref text)
                if text.starts_with("![")
                    && matches!(
                        nodes.last(),
                        Some(Node::Start {
                            element: Element::Paragraph | Element::Item
                        })
                    )
                    && !source[..range.start].ends_with('\\')
                    && &source[range] == text.as_ref()
                    && events.peek().is_some_and(|(next, _)| {
                        matches!(
                            next,
                            Event::End(TagEnd::Paragraph | TagEnd::Item)
                                | Event::Start(
                                    Tag::List(_)
                                        | Tag::BlockQuote(_)
                                        | Tag::CodeBlock(_)
                                        | Tag::Heading { .. }
                                        | Tag::Table(_)
                                        | Tag::HtmlBlock
                                )
                                | Event::Rule
                        )
                    })
                    && let Some((alt, path)) = spaced_image(text) =>
            {
                if depth >= MAXIMUM_DEPTH || nodes.len() + 3 > MAXIMUM_EVENTS {
                    return None;
                }
                nodes.push(Node::Start {
                    element: Element::FileImage { path },
                });
                nodes.push(Node::Text { text: alt.into() });
                Node::End
            }
            event => leaf(event)?,
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
                Node::Text { text } | Node::Code { text } | Node::Math { text, .. } => {
                    text.capacity()
                }
                Node::Start {
                    element:
                        Element::Link { href }
                        | Element::Image { src: href }
                        | Element::FileImage { path: href }
                        | Element::FileImageLink { path: href },
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
    fn strong_delimiters_follow_cjk_and_punctuation_boundaries() {
        for (source, expected) in [
            ("**注意：**内容", "注意："),
            ("**Notice:**内容", "Notice:"),
            ("**事件中间件（waterfall）**实现", "事件中间件（waterfall）"),
            ("**事件中间件(waterfall)**实现", "事件中间件(waterfall)"),
            ("**句号。**后续", "句号。"),
            ("**Period.**后续", "Period."),
            ("**提醒！**继续", "提醒！"),
            ("**Warning!**继续", "Warning!"),
            ("あ**「かな」**カ", "「かな」"),
            ("한**+값+**글", "+값+"),
            ("ㄅ**（音）**ㄆ", "（音）"),
        ] {
            let nodes = parse(source).unwrap();
            assert!(
                nodes.windows(3).any(|run| matches!(run, [
                Node::Start {element: Element::Strong}, Node::Text { text }, Node::End
            ] if text == expected)),
                "{source}"
            );
        }
        for source in [
            "A**(a)**B",
            "文*（词）*文",
            "文__（词）__文",
            r"文\*\*（词）\*\*文",
            "前面的代码：`**注意：**内容`",
        ] {
            assert!(
                !parse(source).unwrap().iter().any(|node| matches!(
                    node,
                    Node::Start {
                        element: Element::Strong | Element::Emphasis
                    }
                )),
                "{source}"
            );
        }
        assert!(!Parser::new("**注意：**内容").any(|e| e == Event::Start(Tag::Strong)));
    }

    #[test]
    fn windows_separators_survive_inline_and_reference_destination_decoding() {
        for (raw, expected) in [
            (r"D:\img\.cache\a.png", r"D:\img\.cache\a.png"),
            (r"docs\_draft.png", r"docs\_draft.png"),
            (r"D:/repo\.hidden\a.png", r"D:/repo\.hidden\a.png"),
            (r"<D:\My Files\.config\a.png>", r"D:\My Files\.config\a.png"),
            (r"D:\R&amp;D\a&#32;b.png", r"D:\R&D\a b.png"),
            (r"docs/\_draft.png", "docs/_draft.png"),
            (r"docs/\图\_draft.png", r"docs/\图\_draft.png"),
            (r"D:\doc\(v2\).png", r"D:\doc\(v2\).png"),
        ] {
            for (label, image) in [("![chart]", true), ("[chart]", false)] {
                for source in [
                    format!("{label}({raw} \"title\")"),
                    format!("{label}[Diagram]\n\n[diagram]: {raw} \"title\""),
                ] {
                    let nodes = parse(&source).unwrap();
                    assert!(
                        nodes.iter().any(|node| match node {
                            Node::Start {
                                element: Element::FileImage { path },
                            } => image && path == expected,
                            Node::Start {
                                element: Element::FileImageLink { path },
                            } => !image && path == expected,
                            _ => false,
                        }),
                        "{source}"
                    );
                }
            }
        }
        let source = r"[remote](https://example.org/r\_s)";
        assert!(parse(source).unwrap().iter().any(|node|
            matches!(node, Node::Start {element: Element::Link {href}} if href == "https://example.org/r_s")));
        assert!(Parser::new(r"[file](docs\_draft.png)").any(|e|
            matches!(e, Event::Start(Tag::Link {dest_url, ..}) if &*dest_url == "docs_draft.png")));
    }
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
        assert!(nodes.iter().any(|node| matches!(node, Node::Start {
            element: Element::Image { src }
        } if src == "https://example.com/image.png")));
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
    fn local_images_resolve_encoded_filenames_and_native_paths_after_markdown_parsing() {
        for (src, expected) in [
            ("./diagram.png", "./diagram.png"),
            ("../a%20b%231%25.png", "../a b#1%.png"),
            ("/home/art/猫.jpg", "/home/art/猫.jpg"),
            ("C:/Photos/a.png", "C:/Photos/a.png"),
        ] {
            let nodes = parse(&format!("![caption]({src})")).unwrap();
            assert!(nodes.iter().any(|node|matches!(node,Node::Start{element:Element::FileImage{path}} if path==expected)));
            let nodes = parse(&format!("[view file]({src})")).unwrap();
            assert!(nodes.iter().any(|node|matches!(node,Node::Start{element:Element::FileImageLink{path}} if path==expected)));
        }
        for invalid in [
            "https://example.com/a",
            "file:///home/private",
            "//another-host/a",
            "bad%zz",
            "bad%00.jpg",
            "data:text/plain,hello",
            "#fragment",
        ] {
            assert!(file_image(invalid).is_none(), "{invalid}");
        }
        assert_eq!(image_link("./A%2EPnG#part").as_deref(), Some("./A.PnG"));
        for value in [
            "../notes.txt",
            "./folder.png/notes",
            "./notes",
            "//server/image.png",
        ] {
            assert!(image_link(value).is_none(), "{value}");
        }
    }

    #[test]
    fn standalone_spaced_image_paths_preserve_escaped_code_and_prose_examples() {
        for (source, expected) in [
            (
                "![Space path](./test workspace/测试图.png)",
                "./test workspace/测试图.png",
            ),
            (
                "![](../shared diagrams/world.svg)",
                "../shared diagrams/world.svg",
            ),
            (
                "> ![Space path](/test workspace/测试图.png)\n",
                "/test workspace/测试图.png",
            ),
            (
                r"- ![Space path](C:\test workspace\测试图.png)",
                r"C:\test workspace\测试图.png",
            ),
            ("- ![Space path](./a b.png)\n  - child", "./a b.png"),
        ] {
            let nodes = parse(source).unwrap();
            assert!(nodes.iter().any(|n| matches!(n, Node::Start{element:Element::FileImage{path}} if path == expected)), "{source}");
        }
        for source in [
            r"\![literal](./a b.png)",
            "`![literal](./a b.png)`",
            "more ![literal](./a b.png)",
            "![literal](./a b.png) more",
            "![literal](a b.png)",
            "![literal](https://example.org/a b.png)",
            "![literal](//network/share/a b.png)",
            "![literal](./a b.txt)",
            "![literal](./a b.png#fragment)",
            "![literal](./a b.png \"title\")",
            "![*caption*](./a b.png)",
            "![literal](./a &amp; b.png)",
            "![literal](./a b.png)\nmore",
        ] {
            assert!(
                !parse(source).unwrap().iter().any(|n| matches!(
                    n,
                    Node::Start {
                        element: Element::FileImage { .. }
                    }
                )),
                "{source}"
            );
        }
    }
    #[test]
    fn math_is_distinct_from_currency_escapes_and_literal_code() {
        let source = r"Cost \$5. Inline $x^2$ and display $$a+b=c$$. `$code$`";
        let nodes = parse(source).unwrap();
        let formulae: Vec<_> = nodes
            .iter()
            .filter_map(|n| match n {
                Node::Math { text, display } => Some((text.as_str(), *display)),
                _ => None,
            })
            .collect();
        assert_eq!(formulae, [("x^2", false), ("a+b=c", true)]);
        assert!(
            nodes
                .iter()
                .any(|n| matches!(n, Node::Code {text} if text == "$code$"))
        );
    }

    #[test]
    fn code_bodies_keep_intentional_blank_lines_for_display_and_copy() {
        for (source, expected) in [
            ("```js\nconst x = 1;\n```", "const x = 1;"),
            ("```js\r\nconst x = 1;\r\n\r\n```", "const x = 1;\n"),
            ("```\n\n```", ""),
            ("```\nongoing", "ongoing"),
        ] {
            let text: String = parse(source)
                .unwrap()
                .into_iter()
                .filter_map(|event| {
                    if let Node::Text { text } = event {
                        Some(text)
                    } else {
                        None
                    }
                })
                .collect();
            assert_eq!(text, expected);
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
