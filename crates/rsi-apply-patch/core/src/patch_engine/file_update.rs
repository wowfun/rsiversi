use std::sync::OnceLock;

use super::parser::UpdateChunk;
use super::{EngineFailure, MAXIMUM_PATCH_FUZZY_MATCHES, MatchKind, PatchFuzzyMatch, SafePath};

#[derive(Clone, Copy, Debug)]
enum LineEnding {
    Lf,
    CrLf,
    Cr,
}

impl LineEnding {
    const fn text(self) -> &'static str {
        match self {
            Self::Lf => "\n",
            Self::CrLf => "\r\n",
            Self::Cr => "\r",
        }
    }
}

#[derive(Clone, Debug)]
struct SourceLine {
    text: String,
    ending: Option<LineEnding>,
}

#[derive(Debug)]
struct SourceFile {
    lines: Vec<SourceLine>,
    preferred: LineEnding,
}

type Replacement = (usize, usize, Vec<String>);

impl SourceFile {
    fn parse(contents: &str) -> Self {
        let mut lines = Vec::new();
        let mut preferred = None;
        let mut start = 0;
        let mut cursor = 0;
        while cursor < contents.len() {
            let (ending, width) = match contents.as_bytes()[cursor] {
                b'\r' if contents.as_bytes().get(cursor + 1) == Some(&b'\n') => {
                    (LineEnding::CrLf, 2)
                }
                b'\r' => (LineEnding::Cr, 1),
                b'\n' => (LineEnding::Lf, 1),
                _ => {
                    cursor += 1;
                    continue;
                }
            };
            preferred.get_or_insert(ending);
            lines.push(SourceLine {
                text: contents[start..cursor].to_owned(),
                ending: Some(ending),
            });
            cursor += width;
            start = cursor;
        }
        if start < contents.len() {
            lines.push(SourceLine {
                text: contents[start..].to_owned(),
                ending: None,
            });
        }
        Self {
            lines,
            preferred: preferred.unwrap_or(LineEnding::Lf),
        }
    }

    fn texts(&self) -> Vec<String> {
        self.lines.iter().map(|line| line.text.clone()).collect()
    }

    fn apply(mut self, replacements: &[Replacement]) -> String {
        let mut source = self.lines.drain(..);
        let mut output = Vec::new();
        let mut source_index = 0;
        for (start, old_len, new_lines) in replacements {
            debug_assert!(*start >= source_index);
            output.extend(source.by_ref().take(*start - source_index));
            source.by_ref().take(*old_len).for_each(drop);
            output.extend(new_lines.iter().map(|text| SourceLine {
                text: text.clone(),
                ending: Some(self.preferred),
            }));
            source_index = *start + *old_len;
        }
        output.extend(source);
        if !output.is_empty() {
            for line in &mut output {
                line.ending.get_or_insert(self.preferred);
            }
        }
        let mut content = String::new();
        for line in output {
            content.push_str(&line.text);
            if let Some(ending) = line.ending {
                content.push_str(ending.text());
            }
        }
        content
    }
}

pub(super) fn derive_update(
    contents: &str,
    path: &SafePath,
    chunks: &[UpdateChunk],
    operation: usize,
    fuzzy_matches: &mut Vec<PatchFuzzyMatch>,
) -> Result<String, EngineFailure> {
    let source = SourceFile::parse(contents);
    let lines = source.texts();
    let views = LineViews::new(&lines);
    let mut replacements = Vec::new();
    let mut line_index = 0;
    let mut previous_end = 0;
    for (hunk, chunk) in chunks.iter().enumerate() {
        if let Some(context) = &chunk.context {
            let (index, kind) =
                seek_sequence(&views, std::slice::from_ref(context), line_index, false)
                    .ok_or_else(|| {
                        EngineFailure::new(
                            "context_not_found",
                            format!("failed to find hunk context in {}", path.text),
                        )
                        .at_hunk(operation, hunk, path)
                    })?;
            audit_match(fuzzy_matches, operation, hunk, path, index, kind)?;
            line_index = index + 1;
        }
        let (start, kind) = if chunk.old_lines.is_empty() {
            (
                if chunk.context.is_some() {
                    line_index
                } else {
                    lines.len()
                },
                MatchKind::Exact,
            )
        } else {
            seek_sequence(&views, &chunk.old_lines, line_index, chunk.eof).ok_or_else(|| {
                EngineFailure::new(
                    "expected_lines_not_found",
                    format!("failed to find expected lines in {}", path.text),
                )
                .at_hunk(operation, hunk, path)
            })?
        };
        if start < previous_end {
            return Err(EngineFailure::new(
                "overlapping_hunks",
                "update hunks are not source-ordered and non-overlapping",
            )
            .at_hunk(operation, hunk, path));
        }
        audit_match(fuzzy_matches, operation, hunk, path, start, kind)?;
        let mut old_start = 0;
        let mut new_start = 0;
        for &(old_context, new_context) in &chunk.context_indices {
            if old_start != old_context || new_start != new_context {
                replacements.push((
                    start + old_start,
                    old_context - old_start,
                    chunk.new_lines[new_start..new_context].to_vec(),
                ));
            }
            old_start = old_context + 1;
            new_start = new_context + 1;
        }
        if old_start != chunk.old_lines.len() || new_start != chunk.new_lines.len() {
            replacements.push((
                start + old_start,
                chunk.old_lines.len() - old_start,
                chunk.new_lines[new_start..].to_vec(),
            ));
        }
        previous_end = start + chunk.old_lines.len();
        line_index = previous_end;
    }
    replacements.sort_by_key(|(start, _, _)| *start);
    for pair in replacements.windows(2) {
        if pair[0].0 + pair[0].1 > pair[1].0 {
            return Err(
                EngineFailure::new("overlapping_hunks", "derived replacements overlap")
                    .at(operation, path),
            );
        }
    }
    Ok(source.apply(&replacements))
}

fn audit_match(
    matches: &mut Vec<PatchFuzzyMatch>,
    operation: usize,
    hunk: usize,
    path: &SafePath,
    source_index: usize,
    kind: MatchKind,
) -> Result<(), EngineFailure> {
    if !kind.is_exact() {
        if matches.len() >= MAXIMUM_PATCH_FUZZY_MATCHES {
            return Err(EngineFailure::new(
                "response_budget",
                format!("patch may report more than {MAXIMUM_PATCH_FUZZY_MATCHES} fuzzy matches"),
            )
            .at_hunk(operation, hunk, path));
        }
        matches.push(PatchFuzzyMatch {
            operation,
            hunk,
            path: path.text.clone(),
            kind,
            source_line: source_index + 1,
        });
    }
    Ok(())
}

struct LineViews<'a> {
    exact: Vec<&'a str>,
    rstrip: OnceLock<Vec<&'a str>>,
    trim: OnceLock<Vec<&'a str>>,
    unicode: OnceLock<Vec<String>>,
}

impl<'a> LineViews<'a> {
    fn new(lines: &'a [String]) -> Self {
        Self {
            exact: lines.iter().map(String::as_str).collect(),
            rstrip: OnceLock::new(),
            trim: OnceLock::new(),
            unicode: OnceLock::new(),
        }
    }
}

fn seek_sequence(
    lines: &LineViews<'_>,
    pattern: &[String],
    start: usize,
    eof: bool,
) -> Option<(usize, MatchKind)> {
    if pattern.is_empty() {
        return Some((start, MatchKind::Exact));
    }
    if pattern.len() > lines.exact.len() || start > lines.exact.len().saturating_sub(pattern.len())
    {
        return None;
    }
    let exact_pattern = pattern.iter().map(String::as_str).collect::<Vec<_>>();
    if let Some(index) = seek_sequence_layer(&lines.exact, &exact_pattern, start, eof) {
        return Some((index, MatchKind::Exact));
    }
    let rstrip_lines = lines
        .rstrip
        .get_or_init(|| lines.exact.iter().map(|line| line.trim_end()).collect());
    let rstrip_pattern = pattern
        .iter()
        .map(|line| line.trim_end())
        .collect::<Vec<_>>();
    if let Some(index) = seek_sequence_layer(rstrip_lines, &rstrip_pattern, start, eof) {
        return Some((index, MatchKind::Rstrip));
    }
    let trim_lines = lines
        .trim
        .get_or_init(|| lines.exact.iter().map(|line| line.trim()).collect());
    let trim_pattern = pattern.iter().map(|line| line.trim()).collect::<Vec<_>>();
    if let Some(index) = seek_sequence_layer(trim_lines, &trim_pattern, start, eof) {
        return Some((index, MatchKind::Trim));
    }
    let unicode_lines = lines.unicode.get_or_init(|| {
        lines
            .exact
            .iter()
            .map(|line| normalize_unicode(line))
            .collect()
    });
    let unicode_pattern = pattern
        .iter()
        .map(|line| normalize_unicode(line))
        .collect::<Vec<_>>();
    if let Some(index) = seek_sequence_layer(unicode_lines, &unicode_pattern, start, eof) {
        return Some((index, MatchKind::Unicode));
    }
    None
}

fn seek_sequence_layer<T: Eq>(
    lines: &[T],
    pattern: &[T],
    start: usize,
    eof: bool,
) -> Option<usize> {
    let end = lines.len().checked_sub(pattern.len())?;
    if eof {
        return (end >= start && lines[end..] == *pattern).then_some(end);
    }
    let mut prefix = vec![0_usize; pattern.len()];
    let mut matched = 0;
    for index in 1..pattern.len() {
        while matched > 0 && pattern[index] != pattern[matched] {
            matched = prefix[matched - 1];
        }
        if pattern[index] == pattern[matched] {
            matched += 1;
            prefix[index] = matched;
        }
    }
    matched = 0;
    for (offset, line) in lines.iter().enumerate().skip(start) {
        while matched > 0 && line != &pattern[matched] {
            matched = prefix[matched - 1];
        }
        if line == &pattern[matched] {
            matched += 1;
            if matched == pattern.len() {
                return Some(offset + 1 - pattern.len());
            }
        }
    }
    None
}

fn normalize_unicode(value: &str) -> String {
    value
        .trim()
        .chars()
        .map(|character| match character {
            '\u{2010}' | '\u{2011}' | '\u{2012}' | '\u{2013}' | '\u{2014}' | '\u{2015}'
            | '\u{2212}' => '-',
            '\u{2018}' | '\u{2019}' | '\u{201a}' | '\u{201b}' => '\'',
            '\u{201c}' | '\u{201d}' | '\u{201e}' | '\u{201f}' => '"',
            '\u{00a0}' | '\u{2002}' | '\u{2003}' | '\u{2004}' | '\u{2005}' | '\u{2006}'
            | '\u{2007}' | '\u{2008}' | '\u{2009}' | '\u{200a}' | '\u{202f}' | '\u{205f}'
            | '\u{3000}' => ' ',
            other => other,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fuzzy_audit_budget_rejects_before_cloning_the_next_path() {
        let path = SafePath::parse("bounded.txt").unwrap();
        let mut matches = vec![
            PatchFuzzyMatch {
                operation: 0,
                hunk: 0,
                path: "bounded.txt".into(),
                kind: MatchKind::Trim,
                source_line: 1,
            };
            MAXIMUM_PATCH_FUZZY_MATCHES
        ];

        let failure = audit_match(&mut matches, 1, 2, &path, 3, MatchKind::Trim).unwrap_err();

        assert_eq!(failure.code, "response_budget");
        assert_eq!(matches.len(), MAXIMUM_PATCH_FUZZY_MATCHES);
    }

    #[test]
    fn sequence_matcher_handles_long_self_overlapping_prefixes() {
        let mut lines = vec!["repeat".to_owned(); 10_000];
        lines.push("needle".to_owned());
        let mut pattern = vec!["repeat".to_owned(); 5_000];
        pattern.push("needle".to_owned());
        let views = LineViews::new(&lines);

        assert_eq!(
            seek_sequence(&views, &pattern, 0, false),
            Some((5_000, MatchKind::Exact))
        );
    }

    #[test]
    fn sequence_matcher_lazily_reuses_whole_file_normalizations() {
        let lines = vec!["exact".to_owned(), "rstrip  ".to_owned()];
        let views = LineViews::new(&lines);

        assert_eq!(
            seek_sequence(&views, &["exact".to_owned()], 0, false),
            Some((0, MatchKind::Exact))
        );
        assert!(views.rstrip.get().is_none());
        assert_eq!(
            seek_sequence(&views, &["rstrip".to_owned()], 0, false),
            Some((1, MatchKind::Rstrip))
        );
        let normalized = views.rstrip.get().unwrap().as_ptr();
        assert_eq!(
            seek_sequence(&views, &["rstrip".to_owned()], 0, false),
            Some((1, MatchKind::Rstrip))
        );
        assert_eq!(views.rstrip.get().unwrap().as_ptr(), normalized);
        assert!(views.trim.get().is_none());
        assert!(views.unicode.get().is_none());
    }
}
