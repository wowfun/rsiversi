use crate::network::Body;
use crate::{MAX_DECODED, MAX_TEXT, RetrievalError as Error};
use encoding_rs::{CoderResult, Encoding, UTF_8};
use html5ever::tokenizer::{
    BufferQueue, TagKind, Token, TokenSink, TokenSinkResult, Tokenizer, TokenizerOpts,
    states::RawKind,
};
use std::{
    cell::{Cell, RefCell},
    io::Read,
    rc::Rc,
};
use tokio_util::sync::CancellationToken;

#[derive(Debug)]
pub(crate) struct DecodedPage {
    pub title: String,
    pub text: String,
    pub truncated: bool,
}
fn bounded_read(mut reader: impl Read, stop: &CancellationToken) -> Result<Vec<u8>, Error> {
    let mut result = Vec::new();
    let mut chunk = [0; 8192];
    loop {
        if stop.is_cancelled() {
            return Err(Error::Cancelled);
        }
        let count = reader.read(&mut chunk).map_err(|_| Error::Decode)?;
        if count == 0 {
            return Ok(result);
        }
        if count > MAX_DECODED.saturating_sub(result.len()) {
            return Err(Error::Capacity);
        }
        result.extend_from_slice(&chunk[..count]);
    }
}
pub(crate) fn decode(body: &Body, stop: &CancellationToken) -> Result<String, Error> {
    let decoded = match body.encoding.trim().to_ascii_lowercase().as_str() {
        "" | "identity" => bounded_read(body.bytes.as_slice(), stop)?,
        "gzip" => bounded_read(
            flate2::read::MultiGzDecoder::new(body.bytes.as_slice()),
            stop,
        )?,
        "br" => bounded_read(brotli::Decompressor::new(body.bytes.as_slice(), 8192), stop)?,
        _ => return Err(Error::UnsupportedContent),
    };
    let charset = body.media.split(';').skip(1).find_map(|part| {
        let (name, value) = part.trim().split_once('=')?;
        name.trim()
            .eq_ignore_ascii_case("charset")
            .then_some(value.trim().trim_matches(['\'', '"']))
    });
    let declared = charset
        .map(|label| Encoding::for_label(label.as_bytes()).ok_or(Error::UnsupportedContent))
        .transpose()?;
    let (encoding, skip) = Encoding::for_bom(&decoded).unwrap_or((declared.unwrap_or(UTF_8), 0));
    let mut transcoder = encoding.new_decoder_without_bom_handling();
    let mut remaining = &decoded[skip..];
    let mut text = String::new();
    let mut chunk = [0; 8192];
    loop {
        if stop.is_cancelled() {
            return Err(Error::Cancelled);
        }
        let (state, read, written, invalid) =
            transcoder.decode_to_utf8(remaining, &mut chunk, true);
        if invalid {
            return Err(Error::Decode);
        }
        if written > MAX_DECODED.saturating_sub(text.len()) {
            return Err(Error::Capacity);
        }
        text.push_str(std::str::from_utf8(&chunk[..written]).map_err(|_| Error::Decode)?);
        remaining = &remaining[read..];
        if state == CoderResult::InputEmpty {
            break;
        }
    }
    Ok(text)
}
fn prefix(text: &str, maximum: usize) -> &str {
    &text[..text.floor_char_boundary(maximum.min(text.len()))]
}
#[derive(Debug, Default)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "Independent tokenizer flags track title mode, whitespace, output truncation and capacity."
)]
struct Extracted {
    text: String,
    title: String,
    truncated: bool,
    title_open: bool,
    hidden: Vec<String>,
    separator: bool,
    capacity: bool,
}
#[derive(Debug, Clone, Default)]
struct Sink {
    data: Rc<RefCell<Extracted>>,
    progress: Rc<Cell<u64>>,
}
impl TokenSink for Sink {
    type Handle = ();
    fn process_token(&self, token: Token, _: u64) -> TokenSinkResult<()> {
        if !matches!(token, Token::ParseError(_)) {
            self.progress.set(self.progress.get() + 1);
        }
        let mut state = self.data.borrow_mut();
        match token {
            Token::TagToken(tag) => {
                let name = tag.name.as_ref();
                if tag.kind == TagKind::EndTag {
                    if state.hidden.last().is_some_and(|last| last == name) {
                        state.hidden.pop();
                    }
                    if name == "title" {
                        state.title_open = false;
                    }
                } else {
                    if name == "title" {
                        state.title_open = state.hidden.is_empty();
                    }
                    if matches!(name, "script" | "style" | "template" | "noscript") {
                        // Only hidden elements need a bounded nesting stack, never a DOM.
                        if state.hidden.len() < 256 {
                            state.hidden.push(name.to_owned());
                        } else {
                            state.capacity = true;
                        }
                    }
                    match name {
                        "script" => return TokenSinkResult::RawData(RawKind::ScriptData),
                        "style" | "noscript" | "iframe" | "xmp" | "noembed" | "noframes" => {
                            return TokenSinkResult::RawData(RawKind::Rawtext);
                        }
                        "title" | "textarea" => return TokenSinkResult::RawData(RawKind::Rcdata),
                        _ => {}
                    }
                }
                if matches!(
                    name,
                    "p" | "div"
                        | "br"
                        | "li"
                        | "tr"
                        | "td"
                        | "th"
                        | "section"
                        | "article"
                        | "header"
                        | "footer"
                        | "h1"
                        | "h2"
                        | "h3"
                        | "h4"
                        | "pre"
                        | "blockquote"
                ) {
                    state.separator = true;
                }
            }
            Token::CharacterTokens(text) if state.hidden.is_empty() => {
                if state.title_open {
                    let left = 1024usize.saturating_sub(state.title.len());
                    let piece = prefix(&text, left);
                    state.title.push_str(piece);
                    state.truncated |= piece.len() < text.len();
                } else {
                    for ch in text.chars() {
                        if ch.is_whitespace() {
                            state.separator = true;
                            continue;
                        }
                        if ch.is_control() {
                            continue;
                        }
                        let separator = usize::from(state.separator && !state.text.is_empty());
                        if ch.len_utf8() + separator > MAX_TEXT.saturating_sub(state.text.len()) {
                            state.truncated = true;
                            break;
                        }
                        if separator != 0 {
                            state.text.push(' ');
                        }
                        state.separator = false;
                        state.text.push(ch);
                    }
                }
            }
            _ => {}
        }
        TokenSinkResult::Continue
    }
}
pub(crate) fn extract(body: &Body, stop: &CancellationToken) -> Result<DecodedPage, Error> {
    let media = body
        .media
        .split(';')
        .next()
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase();
    let html = matches!(media.as_str(), "text/html" | "application/xhtml+xml");
    if !(html
        || media.starts_with("text/")
        || matches!(media.as_str(), "application/json" | "application/xml")
        || media.ends_with("+json")
        || media.ends_with("+xml"))
    {
        return Err(Error::UnsupportedContent);
    }
    let decoded = decode(body, stop)?;
    if !html {
        return Ok(DecodedPage {
            title: String::new(),
            text: prefix(&decoded, MAX_TEXT).to_owned(),
            truncated: decoded.len() > MAX_TEXT,
        });
    }
    let sink = Sink::default();
    let tokenizer = Tokenizer::new(sink.clone(), TokenizerOpts::default());
    let queue = BufferQueue::default();
    let mut offset = 0;
    let mut pending = 0;
    while offset < decoded.len() {
        if stop.is_cancelled() {
            return Err(Error::Cancelled);
        }
        let end = decoded.floor_char_boundary((offset + 4096).min(decoded.len()));
        let previous = sink.progress.get();
        queue.push_back(decoded[offset..end].into());
        let _ = tokenizer.feed(&queue);
        pending = if sink.progress.get() == previous {
            pending + end - offset
        } else {
            end - offset
        };
        // A single pending tag/comment/attribute cannot grow to the full body limit.
        if pending > 64 * 1024 || sink.data.borrow().capacity {
            return Err(Error::Capacity);
        }
        offset = end;
    }
    tokenizer.end();
    let mut result = sink.data.borrow_mut();
    Ok(DecodedPage {
        title: std::mem::take(&mut result.title).trim().to_owned(),
        text: std::mem::take(&mut result.text),
        truncated: result.truncated,
    })
}

#[cfg(test)]
mod tests;
