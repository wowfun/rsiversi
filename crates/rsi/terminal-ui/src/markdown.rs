//! Bounded semantic display text with original byte provenance.
use pulldown_cmark::{Event, Options, Parser, Tag, TagEnd};
use ratatui::style::{Color, Modifier, Style};
use serde::{Deserialize, Serialize};
use std::ops::Range;
use unicode_width::UnicodeWidthStr;

const MAX_BYTES: usize = 1024 * 1024;
const MAX_RUNS: usize = 8192;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Run {
    pub display: Range<usize>,
    pub source: Option<Range<usize>>,
    pub linear: bool,
    pub style: Style,
}
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Document {
    pub text: String,
    pub runs: Vec<Run>,
}
impl Document {
    pub fn bytes(&self) -> usize {
        self.text.capacity() + self.runs.capacity() * size_of::<Run>()
    }
    pub fn validate(&self, source: &str) -> Result<(), &'static str> {
        if self.bytes() > MAX_BYTES
            || self.runs.len() > MAX_RUNS
            || crate::terminal_text(&self.text) != self.text
        {
            return Err("Markdown window bound");
        }
        let mut end = 0;
        for run in &self.runs {
            if run.display.start != end
                || run.display.end <= end
                || !self.text.is_char_boundary(run.display.end)
            {
                return Err("Markdown display range");
            }
            if let Some(range) = &run.source {
                if range.start > range.end
                    || !source.is_char_boundary(range.start)
                    || !source.is_char_boundary(range.end)
                    || (run.linear
                        && (range.len() != run.display.len()
                            || source[range.clone()] != self.text[run.display.clone()]))
                {
                    return Err("Markdown source range");
                }
            } else if run.linear {
                return Err("Markdown decoration source");
            }
            end = run.display.end;
        }
        if end != self.text.len() {
            return Err("Markdown uncovered text");
        }
        Ok(())
    }
    pub fn source_range(&self, display: Range<usize>) -> Option<Range<usize>> {
        let index = self
            .runs
            .partition_point(|r| r.display.end <= display.start);
        let mut source: Option<Range<usize>> = None;
        for run in self.runs[index..]
            .iter()
            .take_while(|run| run.display.start < display.end)
        {
            let Some(range) = &run.source else {
                continue;
            };
            let next = if run.linear {
                range.start + display.start.max(run.display.start) - run.display.start
                    ..range.start + display.end.min(run.display.end) - run.display.start
            } else {
                range.clone()
            };
            source = Some(source.map_or(next.clone(), |old| {
                old.start.min(next.start)..old.end.max(next.end)
            }));
        }
        source
    }

    pub fn source_offset(&self, offset: usize) -> Option<usize> {
        let index = self.runs.partition_point(|r| r.display.end <= offset);
        self.runs[index..]
            .iter()
            .find_map(|run| {
                run.source.as_ref().map(|source| {
                    if run.linear {
                        source.start + offset.saturating_sub(run.display.start)
                    } else {
                        source.start
                    }
                })
            })
            .or_else(|| {
                self.runs[..index]
                    .iter()
                    .rev()
                    .find_map(|r| r.source.as_ref().map(|s| s.end))
            })
    }
    pub fn display_offset(&self, offset: usize) -> usize {
        self.runs
            .iter()
            .find_map(|run| {
                run.source
                    .as_ref()
                    .filter(|range| range.end > offset)
                    .map(|range| {
                        run.display.start
                            + if run.linear {
                                offset.saturating_sub(range.start)
                            } else {
                                0
                            }
                    })
            })
            .unwrap_or(self.text.len())
    }
    fn push(
        &mut self,
        text: &str,
        source: Option<Range<usize>>,
        linear: bool,
        style: Style,
    ) -> Option<()> {
        if text.is_empty() {
            return Some(());
        }
        if self.runs.len() >= MAX_RUNS
            || self.bytes().saturating_add(text.len() + size_of::<Run>()) > MAX_BYTES / 2
        {
            return None;
        }
        let start = self.text.len();
        self.text.push_str(text);
        self.runs.push(Run {
            display: start..self.text.len(),
            source,
            linear,
            style,
        });
        Some(())
    }
    fn decoration(&mut self, text: &str) -> Option<()> {
        self.push(text, None, false, Style::default().fg(Color::DarkGray))
    }
    fn newline(&mut self) -> Option<()> {
        if !self.text.is_empty() && !self.text.ends_with('\n') {
            self.decoration("\n")?;
        }
        Some(())
    }
    fn append(&mut self, other: &Self) -> Option<()> {
        for run in &other.runs {
            self.push(
                &other.text[run.display.clone()],
                run.source.clone(),
                run.linear,
                run.style,
            )?;
        }
        Some(())
    }
}

/// Parsing is independent of source chunk boundaries; failure means render original text.
pub(crate) fn parse(source: &str, width: u16) -> Option<Document> {
    if source.len() > crate::transcript::WINDOW {
        return None;
    }
    let options =
        Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TASKLISTS;
    let mut events = Parser::new_ext(source, options).into_offset_iter();
    let mut writer = Writer::default();
    while let Some((event, range)) = events.next() {
        if matches!(event, Event::Start(Tag::Table(_))) {
            let mut table = Vec::<Vec<Document>>::new();
            let mut row = Vec::new();
            let mut cell = Writer::default();
            let mut in_cell = false;
            let mut finished = false;
            let mut bytes = 0usize;
            for (event, range) in events.by_ref() {
                match event {
                    Event::Start(Tag::TableCell) => {
                        cell = Writer::default();
                        in_cell = true;
                    }
                    Event::End(TagEnd::TableCell) => {
                        bytes = bytes.checked_add(cell.document.bytes())?;
                        if bytes > MAX_BYTES / 2 || row.len() >= 32 {
                            return None;
                        }
                        row.push(std::mem::take(&mut cell.document));
                        in_cell = false;
                    }
                    Event::End(TagEnd::TableRow | TagEnd::TableHead) => {
                        if table.len() >= 1024 {
                            return None;
                        }
                        table.push(std::mem::take(&mut row));
                    }
                    Event::End(TagEnd::Table) => {
                        finished = true;
                        break;
                    }
                    event if in_cell => cell.event(source, event, range)?,
                    _ => {}
                }
            }
            if !finished {
                return None;
            }
            render_table(&mut writer.document, &table, usize::from(width))?;
        } else {
            writer.event(source, event, range)?;
        }
    }
    // Transcript layout owns the space between blocks. Retain source newlines,
    // but do not turn a final Markdown block separator into an extra empty row.
    if let Some(run) = writer.document.runs.last_mut()
        && run.source.is_none()
        && writer.document.text.ends_with('\n')
    {
        writer.document.text.pop();
        run.display.end -= 1;
        if run.display.is_empty() {
            writer.document.runs.pop();
        }
    }
    writer.document.validate(source).ok()?;
    Some(writer.document)
}

#[derive(Default)]
struct Writer {
    document: Document,
    style: Style,
    styles: Vec<Style>,
    lists: Vec<Option<u64>>,
}
impl Writer {
    fn start(&mut self, tag: &Tag<'_>) -> Option<()> {
        if self.styles.len() >= 128 {
            return None;
        }
        self.styles.push(self.style);
        match tag {
            Tag::Heading { .. } => {
                self.document.newline()?;
                self.style = self.style.add_modifier(Modifier::BOLD);
            }
            Tag::Strong => self.style = self.style.add_modifier(Modifier::BOLD),
            Tag::Emphasis => self.style = self.style.add_modifier(Modifier::ITALIC),
            Tag::Strikethrough => {
                self.style = self.style.add_modifier(Modifier::CROSSED_OUT);
            }
            Tag::Link { .. } => {
                self.style = self
                    .style
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::UNDERLINED);
            }
            Tag::CodeBlock(_) => {
                self.document.newline()?;
                self.style = self.style.bg(Color::Indexed(235));
            }
            Tag::BlockQuote => {
                self.document.newline()?;
                self.document.decoration("│ ")?;
            }
            Tag::List(start) => {
                self.document.newline()?;
                self.lists.push(*start);
            }
            Tag::Item => {
                self.document.newline()?;
                self.document
                    .decoration(&"  ".repeat(self.lists.len().saturating_sub(1)))?;
                let label = match self.lists.last_mut() {
                    Some(Some(n)) => {
                        let text = format!("{n}. ");
                        *n = n.saturating_add(1);
                        text
                    }
                    _ => "• ".into(),
                };
                self.document.decoration(&label)?;
            }
            _ => {}
        }
        Some(())
    }
    fn event(&mut self, source: &str, event: Event<'_>, range: Range<usize>) -> Option<()> {
        match event {
            Event::Start(tag) => {
                self.start(&tag)?;
            }
            Event::End(tag) => {
                if matches!(
                    tag,
                    TagEnd::Paragraph
                        | TagEnd::Heading(_)
                        | TagEnd::CodeBlock
                        | TagEnd::BlockQuote
                        | TagEnd::Item
                        | TagEnd::List(_)
                ) {
                    self.document.newline()?;
                }
                if matches!(tag, TagEnd::List(_)) {
                    self.lists.pop();
                }
                self.style = self.styles.pop().unwrap_or_default();
            }
            Event::Text(text) | Event::Html(text) | Event::InlineHtml(text) => {
                self.document.push(
                    &text,
                    Some(range.clone()),
                    source.get(range) == Some(text.as_ref()),
                    self.style,
                )?;
            }
            Event::Code(text) => {
                let raw = &source[range.clone()];
                let mapped = raw.find(text.as_ref()).map_or(range.clone(), |at| {
                    range.start + at..range.start + at + text.len()
                });
                self.document.push(
                    &text,
                    Some(mapped.clone()),
                    source.get(mapped) == Some(text.as_ref()),
                    self.style.bg(Color::Indexed(235)),
                )?;
            }
            Event::SoftBreak => {
                self.document.push(" ", Some(range), false, self.style)?;
            }
            Event::HardBreak => {
                self.document.push("\n", Some(range), false, self.style)?;
            }
            Event::Rule => {
                self.document.newline()?;
                self.document.decoration("───\n")?;
            }
            Event::TaskListMarker(done) => {
                self.document
                    .decoration(if done { "[x] " } else { "[ ] " })?;
            }
            Event::FootnoteReference(_) => {}
        }
        Some(())
    }
}

fn render_table(out: &mut Document, rows: &[Vec<Document>], width: usize) -> Option<()> {
    let columns = rows.iter().map(Vec::len).max().unwrap_or(0);
    if columns == 0 {
        return Some(());
    }
    let widths: Vec<_> = (0..columns)
        .map(|i| {
            rows.iter()
                .filter_map(|row| row.get(i))
                .map(|cell| cell.text.width())
                .max()
                .unwrap_or(0)
        })
        .collect();
    let grid = widths
        .iter()
        .sum::<usize>()
        .saturating_add(3 * columns.saturating_sub(1))
        <= width;
    out.newline()?;
    if grid {
        for (index, row) in rows.iter().enumerate() {
            for (column, cell) in row.iter().enumerate() {
                if column > 0 {
                    out.decoration(" │ ")?;
                }
                out.append(cell)?;
                out.decoration(&" ".repeat(widths[column].saturating_sub(cell.text.width())))?;
            }
            out.newline()?;
            if index == 0 {
                out.decoration(
                    &"─".repeat(widths.iter().sum::<usize>() + 3 * columns.saturating_sub(1)),
                )?;
                out.newline()?;
            }
        }
    } else {
        for (index, row) in rows.iter().enumerate().skip(1) {
            if index > 1 {
                out.decoration("───\n")?;
            }
            for (column, cell) in row.iter().enumerate() {
                if let Some(header) = rows[0].get(column) {
                    out.decoration(header.text.trim())?;
                    out.decoration(": ")?;
                }
                out.append(cell)?;
                out.newline()?;
            }
        }
        if rows.len() == 1 {
            for cell in &rows[0] {
                out.append(cell)?;
                out.newline()?;
            }
        }
    }
    Some(())
}

#[derive(Debug, Default)]
pub(crate) struct Cache {
    entries: std::collections::VecDeque<Cached>,
    bytes: usize,
}
#[derive(Debug)]
struct Cached {
    key: String,
    revision: std::sync::Arc<()>,
    width: u16,
    value: Option<std::sync::Arc<Document>>,
    bytes: usize,
}
impl Cache {
    pub fn get(
        &mut self,
        block: &crate::transcript::Block,
        width: u16,
    ) -> Option<std::sync::Arc<Document>> {
        if let Some(index) = self.entries.iter().position(|entry| {
            entry.key == block.key
                && std::sync::Arc::ptr_eq(&entry.revision, &block.layout_revision)
                && entry.width == width
        }) {
            let entry = self.entries.remove(index)?;
            let value = entry.value.clone();
            self.entries.push_back(entry);
            return value;
        }
        let value = (!block.discarded && !block.pieces.front().is_some_and(|piece| piece.omitted))
            .then(|| parse(&block.text(), width).map(std::sync::Arc::new))
            .flatten();
        let bytes =
            value.as_ref().map_or(0, |value| value.bytes()) + block.key.len() + size_of::<Cached>();
        self.entries.retain(|entry| entry.key != block.key);
        self.bytes = self.entries.iter().map(|entry| entry.bytes).sum();
        if bytes <= MAX_BYTES {
            while self.bytes + bytes > MAX_BYTES || self.entries.len() >= 512 {
                self.bytes -= self.entries.pop_front()?.bytes;
            }
            self.entries.push_back(Cached {
                key: block.key.clone(),
                revision: block.layout_revision.clone(),
                width,
                value: value.clone(),
                bytes,
            });
            self.bytes += bytes;
        }
        value
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn final_synthetic_separator_does_not_add_a_transcript_row() {
        for raw in ["plain", "**bold**", "one\ntwo", "# heading", "- item"] {
            let doc = parse(raw, 80).unwrap();
            assert!(!doc.text.ends_with('\n'), "{raw:?}: {:?}", doc.text);
            doc.validate(raw).unwrap();
        }
        let raw = "```\n  code\n```";
        let doc = parse(raw, 80).unwrap();
        assert_eq!(doc.text, "  code\n");
        assert!(
            doc.source_range(doc.text.len() - 1..doc.text.len())
                .is_some()
        );
    }

    #[test]
    fn semantic_text_preserves_source_provenance_and_decorations() {
        let raw = "# Title\n\n**bold** *斜体* ~~gone~~ &amp; \\*literal\\* [label](https://example.test) ![alt](image.png)\n\n> quote\n\n- [x] task\n\n```rust\n  let x = 1;\n```\n\n<div>literal</div>\n";
        let doc = parse(raw, 80).unwrap();
        assert!(
            doc.text
                .contains("Title\nbold 斜体 gone & *literal* label alt"),
            "{}",
            doc.text
        );
        assert!(doc.text.contains("│ quote"));
        assert!(doc.text.contains("• [x] task"));
        assert!(doc.text.contains("  let x = 1;\n"));
        assert!(doc.text.contains("<div>literal</div>"));
        assert!(!doc.text.contains("```"));
        let offset = doc.text.find('&').unwrap();
        assert_eq!(&raw[doc.source_range(offset..offset + 1).unwrap()], "&amp;");
        let bullet = doc.text.find('•').unwrap();
        assert!(doc.source_range(bullet..bullet + '•'.len_utf8()).is_none());
        let strong = doc
            .runs
            .iter()
            .find(|run| &doc.text[run.display.clone()] == "bold")
            .unwrap();
        assert!(strong.style.add_modifier.contains(Modifier::BOLD));
    }
    #[test]
    fn narrow_tables_keep_values_and_source_order_without_border_authority() {
        let raw = "| Name | Description |\n|---|---|\n| A | long description |\n| 中 | 👩‍💻 value |";
        let wide = parse(raw, 80).unwrap();
        assert!(wide.text.contains('│'));
        let narrow = parse(raw, 16).unwrap();
        assert!(
            narrow
                .text
                .contains("Name: A\nDescription: long description")
        );
        assert!(narrow.text.contains("Name: 中\nDescription: 👩‍💻 value"));
        for run in narrow.runs.iter().filter(|r| r.source.is_some()) {
            assert_eq!(
                raw[run.source.clone().unwrap()],
                narrow.text[run.display.clone()]
            );
        }
    }
    #[test]
    fn streaming_fences_and_capacity_fallback_are_deterministic() {
        for raw in [
            "```rust\n  **literal",
            "```rust\n  **literal**\n```",
            "**incomplete",
            "**complete**",
        ] {
            let doc = parse(raw, 40).unwrap();
            doc.validate(raw).unwrap();
        }
        assert!(parse(&"**x** ".repeat(MAX_RUNS), 80).is_none());
        assert!(parse(&"x".repeat(crate::transcript::WINDOW + 1), 80).is_none());
    }
    #[test]
    fn grapheme_across_style_runs_has_one_complete_original_range() {
        let raw = "**e**\u{301}";
        let doc = parse(raw, 80).unwrap();
        assert_eq!(&raw[doc.source_range(0..3).unwrap()], "e**\u{301}");
    }
    #[test]
    fn forged_display_or_source_ranges_are_rejected() {
        let raw = "**中文**";
        let doc = parse(raw, 80).unwrap();
        let mut invalid = doc.clone();
        invalid.runs[0].source = Some(usize::MAX..usize::MAX);
        assert!(invalid.validate(raw).is_err());
        let mut invalid = doc;
        invalid.runs[0].display.end = usize::MAX;
        assert!(invalid.validate(raw).is_err());
    }
}
