//! Closed Markdown rendering and parsed HTML/CSS resource references.
use html5ever::tokenizer::{
    BufferQueue, TagKind, Token, TokenSink, TokenSinkResult, Tokenizer, TokenizerOpts,
    states::RawKind,
};
use rsi_files_protocol::RelativePath;
use std::fmt::Write as _;
use std::{cell::RefCell, collections::BTreeMap, rc::Rc};

pub(crate) const MARKER: &str = "rsi-preview-resource:";
// Two core sources (main and document) share the 32-source model bound.
const MAXIMUM_RESOURCES: usize = 30;
#[derive(Clone, Debug)]
pub(crate) enum Location {
    File(RelativePath),
    Embedded { mime: String, bytes: Vec<u8> },
}
#[derive(Clone, Debug)]
pub(crate) struct Reference {
    pub location: Location,
    pub kind: &'static str,
}
#[derive(Default, Debug)]
pub(crate) struct Resources {
    pub entries: Vec<Reference>,
    names: BTreeMap<Vec<u8>, usize>,
    pub diagnostics: Vec<String>,
}
impl Resources {
    pub fn reference(&mut self, base: &RelativePath, value: &str, kind: &'static str) -> String {
        if value.starts_with('#') || value.is_empty() {
            return value.into();
        }
        if value.starts_with("https://") || value.starts_with("http://") {
            return value.into();
        }
        let located = if value
            .get(..5)
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case("data:"))
        {
            embedded(value).map(|location| (value.as_bytes().to_vec(), location, String::new()))
        } else {
            relative(base, value)
                .map(|(path, fragment)| (path.as_bytes().to_vec(), Location::File(path), fragment))
        };
        match located {
            Ok((key, location, fragment)) => {
                let index = if let Some(index) = self.names.get(&key) {
                    *index
                } else {
                    if self.entries.len() >= MAXIMUM_RESOURCES {
                        self.diagnostic(&format!(
                            "Document exceeds {MAXIMUM_RESOURCES} local resources"
                        ));
                        return "about:blank".into();
                    }
                    let index = self.entries.len();
                    self.names.insert(key, index);
                    self.entries.push(Reference { location, kind });
                    index
                };
                format!("{MARKER}{index}{fragment}")
            }
            Err(error) => {
                self.diagnostic(error);
                "about:blank".into()
            }
        }
    }
    pub fn diagnostic(&mut self, message: &str) {
        if self.diagnostics.len() < 32 && !self.diagnostics.iter().any(|old| old == message) {
            self.diagnostics.push(message.into());
        }
    }
}
fn embedded(value: &str) -> Result<Location, &'static str> {
    use base64::Engine as _;
    const MAXIMUM: usize = 4 * 1024 * 1024;
    let (metadata, encoded) = value[5..]
        .split_once(',')
        .ok_or("Invalid embedded resource")?;
    if encoded.len() > MAXIMUM * 3 {
        return Err("Embedded resource exceeds 4 MiB");
    }
    let mut parts = metadata.split(';');
    let mime = parts.next().unwrap_or_default().to_ascii_lowercase();
    if !matches!(
        mime.as_str(),
        "image/png"
            | "image/jpeg"
            | "image/gif"
            | "image/webp"
            | "image/bmp"
            | "image/x-icon"
            | "image/vnd.microsoft.icon"
            | "image/svg+xml"
            | "font/woff"
            | "font/woff2"
            | "font/ttf"
            | "font/otf"
    ) {
        return Err("Unsupported embedded resource type");
    }
    let parameters = parts.collect::<Vec<_>>();
    let base64 = parameters
        .last()
        .is_some_and(|part| part.eq_ignore_ascii_case("base64"));
    if parameters.iter().any(|part| {
        !part.eq_ignore_ascii_case("base64") && !part.eq_ignore_ascii_case("charset=utf-8")
    }) {
        return Err("Unsupported embedded resource parameters");
    }
    let decoded = percent_encoding::percent_decode_str(encoded).collect::<Vec<_>>();
    let bytes = if base64 {
        base64::engine::general_purpose::STANDARD
            .decode(decoded)
            .map_err(|_| "Invalid embedded base64")?
    } else {
        decoded
    };
    if bytes.len() > MAXIMUM {
        return Err("Embedded resource exceeds 4 MiB");
    }
    Ok(Location::Embedded { mime, bytes })
}
pub(crate) fn relative(
    base: &RelativePath,
    value: &str,
) -> Result<(RelativePath, String), &'static str> {
    let (path, fragment) = value
        .split_once('#')
        .map_or((value, String::new()), |(path, fragment)| {
            (path, format!("#{fragment}"))
        });
    let path = path.split('?').next().unwrap_or(path);
    let decoded = percent_encoding::percent_decode_str(path).collect::<Vec<_>>();
    if decoded.contains(&0)
        || decoded.contains(&b'\\')
        || decoded.contains(&b':')
        || decoded.starts_with(b"//")
    {
        return Err("Unsupported or unsafe resource URL");
    }
    let mut parts: Vec<Vec<u8>> = if decoded.starts_with(b"/") {
        Vec::new()
    } else {
        base.as_bytes()
            .split(|b| *b == b'/')
            .map(<[u8]>::to_vec)
            .collect()
    };
    if !decoded.starts_with(b"/") {
        parts.pop();
    }
    for part in decoded.split(|b| *b == b'/') {
        match part {
            b"" | b"." => {}
            b".." => {
                if parts.pop().is_none() {
                    return Err("Resource escapes the Session workspace");
                }
            }
            _ => parts.push(part.to_vec()),
        }
    }
    RelativePath::new(&parts.join(&b'/'))
        .map(|path| (path, fragment))
        .map_err(|_| "Invalid workspace resource path")
}
fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

pub(crate) fn markdown(
    text: &str,
    base: &RelativePath,
    resources: &mut Resources,
) -> Result<String, String> {
    use pulldown_cmark::{Event, Options, Parser, Tag};
    let mut events = Vec::new();
    let mut depth = 0usize;
    for event in Parser::new_ext(
        text,
        Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TASKLISTS,
    ) {
        if events.len() >= 65_536 {
            return Err("Markdown exceeds its event budget; use Source".into());
        }
        match &event {
            Event::Start(_) => {
                depth += 1;
                if depth > 32 {
                    return Err("Markdown exceeds nesting limit; use Source".into());
                }
            }
            Event::End(_) => depth = depth.saturating_sub(1),
            _ => {}
        }
        events.push(match event {
            Event::Html(text) | Event::InlineHtml(text) => Event::Text(text),
            Event::Start(Tag::Image {
                link_type,
                dest_url,
                title,
                id,
            }) => {
                let url = if dest_url.starts_with("http://") || dest_url.starts_with("https://") {
                    resources.diagnostic("Remote Markdown images are not loaded automatically");
                    "about:blank".into()
                } else {
                    resources.reference(base, &dest_url, "image")
                };
                Event::Start(Tag::Image {
                    link_type,
                    dest_url: url.into(),
                    title,
                    id,
                })
            }
            Event::Start(Tag::Link {
                link_type,
                dest_url,
                title,
                id,
            }) => {
                let url = if dest_url.starts_with("https://")
                    || dest_url.starts_with("http://")
                    || dest_url.starts_with('#')
                {
                    dest_url
                } else {
                    "#".into()
                };
                Event::Start(Tag::Link {
                    link_type,
                    dest_url: url,
                    title,
                    id,
                })
            }
            other => other,
        });
    }
    let mut html = String::new();
    pulldown_cmark::html::push_html(&mut html, events.into_iter());
    if html.len() > 8 * 1024 * 1024 {
        return Err("Rendered Markdown exceeds its byte budget".into());
    }
    Ok(html)
}

pub(crate) fn css(text: &str, base: &RelativePath, resources: &mut Resources) -> String {
    use cssparser::{Parser, ParserInput, Token};
    fn walk<'i>(
        parser: &mut Parser<'i, '_>,
        base: &RelativePath,
        resources: &mut Resources,
        out: &mut String,
        depth: usize,
    ) {
        if depth > 32 {
            resources.diagnostic("CSS nesting exceeds 32");
            return;
        }
        let mut import = false;
        while !parser.is_exhausted() {
            let start = parser.position();
            let Ok(token) = parser.next_including_whitespace_and_comments().cloned() else {
                break;
            };
            match token {
                Token::UnquotedUrl(url) => {
                    let url = resources.reference(base, &url, if import { "css" } else { "asset" });
                    let _ = write!(out, "url(\"{}\")", url.replace('"', "%22"));
                    import = false;
                }
                Token::QuotedString(url) if import => {
                    let url = resources.reference(base, &url, "css");
                    let _ = write!(out, "\"{}\"", url.replace('"', "%22"));
                    import = false;
                }
                Token::Function(name) if name.eq_ignore_ascii_case("url") => {
                    let value: Result<String, cssparser::ParseError<'i, ()>> = parser
                        .parse_nested_block(|p| {
                            p.expect_string()
                                .map(ToString::to_string)
                                .map_err(Into::into)
                        });
                    match value {
                        Ok(value) => {
                            let url = resources.reference(
                                base,
                                &value,
                                if import { "css" } else { "asset" },
                            );
                            let _ = write!(out, "url(\"{}\")", url.replace('"', "%22"));
                        }
                        Err(_) => resources.diagnostic("Invalid CSS URL"),
                    }
                    import = false;
                }
                Token::Function(_)
                | Token::ParenthesisBlock
                | Token::SquareBracketBlock
                | Token::CurlyBracketBlock => {
                    out.push_str(parser.slice_from(start));
                    let close = match token {
                        Token::SquareBracketBlock => ']',
                        Token::CurlyBracketBlock => '}',
                        _ => ')',
                    };
                    let _: Result<(), cssparser::ParseError<'i, ()>> =
                        parser.parse_nested_block(|p| {
                            walk(p, base, resources, out, depth + 1);
                            Ok(())
                        });
                    out.push(close);
                }
                _ => {
                    if let Token::AtKeyword(name) = &token {
                        import = name.eq_ignore_ascii_case("import");
                    } else if !matches!(token, Token::WhiteSpace(_) | Token::Comment(_)) {
                        import = false;
                    }
                    out.push_str(parser.slice_from(start));
                }
            }
        }
    }
    let mut input = ParserInput::new(text);
    let mut parser = Parser::new(&mut input);
    let mut out = String::new();
    walk(&mut parser, base, resources, &mut out, 0);
    out
}

#[expect(
    clippy::too_many_lines,
    reason = "Tokenizer retains explicit bounded handling of HTML token kinds and resource attributes"
)]
pub(crate) fn html(
    text: &str,
    base: &RelativePath,
    resources: &mut Resources,
) -> Result<String, String> {
    #[derive(Default)]
    struct State {
        out: String,
        raw: Option<String>,
        style: String,
        events: usize,
    }
    struct Sink<'a> {
        state: Rc<RefCell<State>>,
        resources: RefCell<&'a mut Resources>,
        base: &'a RelativePath,
    }
    impl TokenSink for Sink<'_> {
        type Handle = ();
        #[expect(
            clippy::too_many_lines,
            reason = "Each tag attribute is interpreted in one tokenizer sink"
        )]
        fn process_token(&self, token: Token, _: u64) -> TokenSinkResult<()> {
            let mut state = self.state.borrow_mut();
            state.events += 1;
            if state.events > 131_072 || state.out.len() > 8 * 1024 * 1024 {
                return TokenSinkResult::Continue;
            }
            let mut resources = self.resources.borrow_mut();
            match token {
                Token::TagToken(mut tag) => {
                    let name = tag.name.to_string();
                    if name == "base" {
                        resources.diagnostic("HTML base elements are unsupported; references use the document directory");
                        return TokenSinkResult::Continue;
                    }
                    if tag.kind == TagKind::EndTag {
                        if name == "style" {
                            let style = std::mem::take(&mut state.style);
                            state.out.push_str(&css(&style, self.base, &mut resources));
                        }
                        state.raw = None;
                        let _ = write!(state.out, "</{name}>");
                    } else {
                        let module = name == "script"
                            && tag.attrs.iter().any(|attr| {
                                attr.name.local.as_ref() == "type"
                                    && attr.value.eq_ignore_ascii_case("module")
                            });
                        if module {
                            resources.diagnostic(
                                "ES modules are outside the single-page preview contract",
                            );
                        }
                        let stylesheet = name == "link"
                            && tag.attrs.iter().any(|attr| {
                                attr.name.local.as_ref() == "rel"
                                    && attr
                                        .value
                                        .split_ascii_whitespace()
                                        .any(|rel| rel.eq_ignore_ascii_case("stylesheet"))
                            });
                        for attr in &mut tag.attrs {
                            let key = attr.name.local.as_ref();
                            let value = attr.value.to_string();
                            if key == "style" {
                                attr.value = css(&value, self.base, &mut resources).into();
                            } else if key == "src" && name == "script" && !module {
                                attr.value =
                                    resources.reference(self.base, &value, "script").into();
                            } else if key == "href" && stylesheet {
                                attr.value = resources.reference(self.base, &value, "css").into();
                            } else if (key == "src" && name == "img")
                                || (key == "poster" && name == "video")
                            {
                                attr.value = resources.reference(self.base, &value, "image").into();
                            } else if key == "srcset" && (name == "img" || name == "source") {
                                if value.contains("data:") {
                                    resources.diagnostic("Data URLs in srcset are unsupported");
                                    attr.value = "".into();
                                } else {
                                    attr.value = value
                                        .split(',')
                                        .map(|item| {
                                            let mut pieces = item.split_ascii_whitespace();
                                            let url = resources.reference(
                                                self.base,
                                                pieces.next().unwrap_or(""),
                                                "image",
                                            );
                                            format!(
                                                "{url} {}",
                                                pieces.collect::<Vec<_>>().join(" ")
                                            )
                                        })
                                        .collect::<Vec<_>>()
                                        .join(", ")
                                        .into();
                                }
                            }
                            if module && key == "type" {
                                attr.value = "application/rsi-disabled-module".into();
                            }
                        }
                        state.out.push('<');
                        state.out.push_str(&name);
                        for attr in &tag.attrs {
                            state.out.push(' ');
                            state.out.push_str(&attr.name.local);
                            state.out.push_str("=\"");
                            state.out.push_str(&escape(&attr.value));
                            state.out.push('"');
                        }
                        state.out.push('>');
                        match name.as_str() {
                            "script" => {
                                state.raw = Some(name);
                                return TokenSinkResult::RawData(RawKind::ScriptData);
                            }
                            "style" => {
                                state.raw = Some(name);
                                return TokenSinkResult::RawData(RawKind::Rawtext);
                            }
                            "textarea" | "title" => {
                                return TokenSinkResult::RawData(RawKind::Rcdata);
                            }
                            _ => {}
                        }
                    }
                }
                Token::CharacterTokens(value) => match state.raw.as_deref() {
                    Some("style") => state.style.push_str(&value),
                    Some(_) => state.out.push_str(&value),
                    None => state.out.push_str(&escape(&value)),
                },
                Token::CommentToken(value) => {
                    state.out.push_str("<!--");
                    state.out.push_str(&value);
                    state.out.push_str("-->");
                }
                Token::DoctypeToken(_) => state.out.push_str("<!doctype html>"),
                _ => {}
            }
            TokenSinkResult::Continue
        }
    }
    let state = Rc::new(RefCell::new(State::default()));
    let sink = Sink {
        state: state.clone(),
        resources: RefCell::new(resources),
        base,
    };
    let tokenizer = Tokenizer::new(sink, TokenizerOpts::default());
    let input = BufferQueue::default();
    input.push_back(text.into());
    let _ = tokenizer.feed(&input);
    tokenizer.end();
    drop(tokenizer);
    let state = Rc::try_unwrap(state)
        .ok()
        .expect("tokenizer dropped")
        .into_inner();
    if state.events > 131_072 || state.out.len() > 8 * 1024 * 1024 {
        return Err("HTML exceeds rendering budget; use Source".into());
    }
    Ok(state.out)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn path(value: &str) -> RelativePath {
        RelativePath::new(value.as_bytes()).unwrap()
    }
    #[test]
    fn resource_paths_are_workspace_contained() {
        assert_eq!(
            relative(&path("docs/report.html"), "../images/a.png#part").unwrap(),
            (path("images/a.png"), "#part".into())
        );
        for value in [
            "../../secret",
            "%2e%2e/%2e%2e/secret",
            "file:///secret",
            "//host/x",
            "%2f%2fhost/x",
            "a%00b",
        ] {
            assert!(relative(&path("docs/a.html"), value).is_err(), "{value}");
        }
    }
    #[test]
    fn markdown_preserves_raw_html_as_text_and_collects_local_images() {
        let mut resources = Resources::default();
        let html=markdown("<script>alert(1)</script>\n\n|A|B|\n|-|-|\n|x|y|\n\n![alt](../a.png)\n\n```rust\nfn main() {}\n```",&path("docs/a.md"),&mut resources).unwrap();
        assert!(html.contains("&lt;script&gt;"));
        assert!(html.contains("<table>"));
        assert!(html.contains("language-rust"));
        assert!(
            matches!(&resources.entries[0].location, Location::File(file) if file == &path("a.png"))
        );
        assert!(!html.contains("<script>"));
    }
    #[test]
    fn embedded_images_join_the_bounded_resource_package() {
        let base = path("docs/report.html");
        for input in [
            "<img src='data:image/png;base64,iVBORw0KGgo='>",
            "<style>body { background: url('data:image/png;base64,iVBORw0KGgo='); }</style>",
        ] {
            let mut resources = Resources::default();
            let rendered = html(input, &base, &mut resources).unwrap();
            assert!(!rendered.contains("data:image"));
            assert!(rendered.contains("rsi-preview-resource:0"));
            assert!(
                matches!(&resources.entries[0].location, Location::Embedded { mime, bytes } if mime == "image/png" && bytes == b"\x89PNG\r\n\x1a\n")
            );
        }
        let mut resources = Resources::default();
        let rendered = markdown(
            "![pixel](data:image/png;base64,iVBORw0KGgo=)",
            &base,
            &mut resources,
        )
        .unwrap();
        assert!(rendered.contains("rsi-preview-resource:0"));
        for invalid in [
            "data:image/png;base64,???",
            "data:text/html,%3Cscript%3E",
            "data:image/png;unknown,a",
        ] {
            assert_eq!(resources.reference(&base, invalid, "image"), "about:blank");
        }
        assert!(embedded("data:image/svg+xml,%3Csvg%3E").is_ok());
        assert!(
            embedded(&format!(
                "data:image/png,{}",
                "x".repeat(4 * 1024 * 1024 + 1)
            ))
            .is_err()
        );
    }
    #[test]
    fn html_and_css_use_parsers_and_preserve_script_bodies() {
        let mut resources = Resources::default();
        let html=html("<html><head><style>@import 'theme.css'; .x { background: url(\"../x.png\"); }</style></head><body><script>if (1 < 2) window.test='OK';</script><script src='app.js'></script><img src='x.png'></body></html>",&path("docs/a.html"),&mut resources).unwrap();
        assert!(html.contains("if (1 < 2)"));
        assert!(html.contains(MARKER));
        assert_eq!(resources.entries.len(), 4);
        assert_eq!(resources.entries[0].kind, "css");
    }
}
