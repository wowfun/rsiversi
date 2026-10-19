use super::{Diagnostic, line_number, markdown_links, parser, resolve_link_target, visit_files};
use pulldown_cmark::{CodeBlockKind, Event, Tag, TagEnd};
use std::{collections::BTreeMap, fs, path::Path};

#[derive(Default)]
struct Examples {
    identities: BTreeMap<String, usize>,
    commands: BTreeMap<String, (String, String, String)>,
}

fn user_paths(repository: &Path, diagnostics: &mut Vec<Diagnostic>) -> Vec<std::path::PathBuf> {
    let directory = repository.join("docs/user");
    match directory.try_exists() {
        Ok(false) => return Vec::new(),
        Err(error) => {
            diagnostics.push(Diagnostic::new(
                repository,
                &directory,
                1,
                error.to_string(),
            ));
            return Vec::new();
        }
        Ok(true) => {}
    }
    let mut paths = Vec::new();
    visit_files(
        repository,
        &directory,
        ".md",
        false,
        &mut paths,
        diagnostics,
    );
    paths
}

pub(super) fn validate(repository: &Path, diagnostics: &mut Vec<Diagnostic>) {
    let paths = user_paths(repository, diagnostics);
    let mut documents = BTreeMap::new();
    let mut identities = BTreeMap::new();
    for path in paths {
        let contents = match fs::read_to_string(&path) {
            Ok(contents) => contents,
            Err(error) => {
                diagnostics.push(Diagnostic::new(
                    repository,
                    &path,
                    1,
                    format!("could not read user tutorial: {error}"),
                ));
                continue;
            }
        };
        let stem = path.file_stem().expect("Markdown file").to_string_lossy();
        let english = !stem.ends_with(".zh");
        let counterpart = stem
            .strip_suffix(".zh")
            .map_or_else(|| format!("{stem}.zh.md"), |stem| format!("{stem}.md"));
        let paired = path.with_file_name(&counterpart);
        if !paired.is_file() {
            diagnostics.push(Diagnostic::new(
                repository,
                &path,
                1,
                format!("user tutorial requires its English/Chinese pair: {counterpart}"),
            ));
        } else if !markdown_links(&contents).iter().any(|link| {
            !link.is_image
                && resolve_link_target(repository, &path, &link.destination)
                    .is_ok_and(|(target, _)| target == paired)
        }) {
            diagnostics.push(Diagnostic::new(
                repository,
                &path,
                1,
                format!("user tutorial must link to its translation: {counterpart}"),
            ));
        }
        let mut report =
            |line, message| diagnostics.push(Diagnostic::new(repository, &path, line, message));
        let examples = examples(&contents, &mut report);
        if english {
            for (id, line) in &examples.identities {
                if let Some(previous) = identities.insert(id.clone(), path.clone()) {
                    report(
                        *line,
                        format!(
                            "duplicate user example id: {id} (also in {})",
                            previous.display()
                        ),
                    );
                }
            }
        }
        documents.insert(path, (english, paired, examples));
    }
    for (path, (english, paired, original)) in &documents {
        if !english {
            continue;
        }
        let Some((_, _, translated)) = documents.get(paired) else {
            continue;
        };
        let ids = original
            .commands
            .keys()
            .chain(translated.commands.keys())
            .collect::<std::collections::BTreeSet<_>>();
        for id in ids {
            if original.commands.get(id) != translated.commands.get(id) {
                let (source, line) = translated
                    .identities
                    .get(id)
                    .map_or_else(|| (path, original.identities[id]), |line| (paired, *line));
                diagnostics.push(Diagnostic::new(
                    repository,
                    source,
                    line,
                    format!("translated command examples differ for id `{id}`"),
                ));
            }
        }
    }
}

fn fenced_blocks(
    contents: &str,
    markers: &mut BTreeMap<usize, String>,
) -> BTreeMap<usize, (String, String, bool)> {
    let mut blocks = BTreeMap::new();
    let mut block = None;
    for (event, range) in parser(contents).into_offset_iter() {
        match event {
            Event::Html(_) | Event::InlineHtml(_) => {
                collect_markers(contents, range, markers);
            }
            Event::Start(Tag::CodeBlock(CodeBlockKind::Fenced(info))) => {
                block = Some((
                    line_number(contents, range.start),
                    contents[range.start..]
                        .find('\n')
                        .map_or(range.end, |offset| range.start + offset + 1),
                    info.split_whitespace()
                        .next()
                        .unwrap_or_default()
                        .to_ascii_lowercase(),
                    String::new(),
                ));
            }
            Event::Text(text) if block.is_some() => {
                let block = block.as_mut().unwrap();
                block.1 = range.end;
                block.3.push_str(&text);
            }
            Event::End(TagEnd::CodeBlock) => {
                if let Some((line, content_end, language, command)) = block.take() {
                    blocks.insert(
                        line,
                        (
                            language,
                            command.replace("\r\n", "\n"),
                            // CommonMark consumes a closing fence without Text.
                            // Empty trailing container prefixes can also remain;
                            // only a consumed delimiter establishes closure.
                            contents.get(content_end..range.end).is_some_and(|tail| {
                                let tail = tail.trim_end();
                                tail.ends_with("```") || tail.ends_with("~~~")
                            }),
                        ),
                    );
                }
            }
            _ => {}
        }
    }
    blocks
}

fn examples(contents: &str, report: &mut impl FnMut(usize, String)) -> Examples {
    let mut markers = BTreeMap::new();
    let blocks = fenced_blocks(contents, &mut markers);
    let mut result = Examples::default();
    for (line, marker) in markers {
        let Some(fields) = marker
            .strip_prefix("<!-- rsi-example ")
            .and_then(|s| s.strip_suffix(" -->"))
        else {
            report(line, "invalid user example marker".into());
            continue;
        };
        let fields = fields.split_whitespace().collect::<Vec<_>>();
        let [id, platform] = fields.as_slice() else {
            report(line, "user example needs id and platform".into());
            continue;
        };
        let Some(id) = id.strip_prefix("id=").filter(|id| {
            !id.is_empty()
                && id
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        }) else {
            report(line, "invalid user example id".into());
            continue;
        };
        if result.identities.insert(id.to_owned(), line).is_some() {
            report(line, format!("duplicate user example id: {id}"));
        }
        let Some(platform) = platform
            .strip_prefix("platform=")
            .filter(|p| matches!(*p, "windows" | "linux" | "git-bash" | "all"))
        else {
            report(line, format!("invalid user example platform for id `{id}`"));
            continue;
        };
        let Some((language, command, closed)) = blocks.get(&(line + 1)) else {
            report(
                line,
                format!("user example `{id}` must immediately precede a fenced command"),
            );
            continue;
        };
        if !closed {
            report(line + 1, format!("unterminated user example `{id}`"));
            continue;
        }
        if !matches!(
            (platform, language.as_str()),
            ("windows" | "all", "powershell") | ("linux" | "git-bash" | "all", "bash")
        ) {
            report(
                line + 1,
                format!("user example `{id}` fence language does not match its platform"),
            );
            continue;
        }
        result.commands.insert(
            id.to_owned(),
            (platform.to_owned(), language.clone(), command.clone()),
        );
    }
    result
}

fn collect_markers(
    contents: &str,
    range: std::ops::Range<usize>,
    markers: &mut BTreeMap<usize, String>,
) {
    for (offset, line) in
        contents[range.clone()]
            .split_inclusive('\n')
            .scan(range.start, |offset, line| {
                let start = *offset;
                *offset += line.len();
                Some((start, line))
            })
    {
        if line.trim_start().starts_with("<!-- rsi-example") {
            markers.insert(line_number(contents, offset), line.trim().to_owned());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn validate_pair(original: &[u8], translated: &[u8]) -> Vec<String> {
        let root = tempfile::tempdir().unwrap();
        let directory = root.path().join("docs/user/nested");
        fs::create_dir_all(&directory).unwrap();
        fs::write(directory.join("start.md"), original).unwrap();
        fs::write(directory.join("start.zh.md"), translated).unwrap();
        let mut diagnostics = Vec::new();
        validate(root.path(), &mut diagnostics);
        diagnostics.into_iter().map(|d| d.render()).collect()
    }

    #[test]
    fn commonmark_links_and_fences_are_compared_semantically() {
        let original = "[中文](./start.zh.md \"中文版\")\n\n<!-- rsi-example id=a platform=all -->\n   ~~~~BASH {.x}\n   echo ok\n   ~~~~  \n";
        let translated = "[English][pair]\n\n[pair]: start.md\n\n<!-- rsi-example id=a platform=all -->\n````bash\necho ok\n````\n";
        assert!(validate_pair(original.as_bytes(), translated.as_bytes()).is_empty());
        for fake in [
            "`[中文](start.zh.md)`",
            "![中文](start.zh.md)",
            "```md\n[中文](start.zh.md)\n```",
        ] {
            let errors = validate_pair(fake.as_bytes(), b"[English](start.md)");
            assert!(
                errors.iter().any(|e| e.contains("link to its translation")),
                "{fake}: {errors:?}"
            );
        }
    }

    #[test]
    fn container_fences_and_unterminated_indentation_follow_commonmark() {
        let marker = "<!-- rsi-example id=a platform=all -->";
        for source in [
            format!("> {marker}\n> ```bash\n> echo ok\n> ```\n"),
            format!("- {marker}\n  ~~~bash\n  echo ok\n  ~~~\n"),
            format!("{marker}\n```bash\n```\n"),
            format!("> {marker}\n> ```bash\n>\n> ```"),
        ] {
            let mut errors = Vec::new();
            let parsed = examples(&source, &mut |_, error| errors.push(error));
            assert!(errors.is_empty(), "{source}: {errors:?}");
            assert!(parsed.commands.contains_key("a"));
        }
        for source in [
            format!("{marker}\n```bash\necho ok\n    ```\n"),
            format!("> {marker}\n> ```bash\n> echo ok\n\noutside\n"),
            format!("{marker}\n```bash"),
            format!("> {marker}\n> ```bash\n> echo ok\n>"),
            format!("> {marker}\n> ```bash\n>\n"),
            format!("- {marker}\n  ```bash\n  echo ok\n  \n"),
        ] {
            let mut errors = Vec::new();
            let parsed = examples(&source, &mut |_, error| errors.push(error));
            assert!(parsed.commands.is_empty(), "{source}");
            assert!(
                errors.iter().any(|e| e.contains("unterminated")),
                "{source}: {errors:?}"
            );
        }
    }

    #[test]
    fn marker_errors_do_not_suppress_other_comparisons_or_id_checks() {
        let root = tempfile::tempdir().unwrap();
        let directory = root.path().join("docs/user/sub");
        fs::create_dir_all(&directory).unwrap();
        let example = "<!-- rsi-example id=a platform=all -->\n```bash\necho ok\n```\n";
        fs::write(
            directory.join("start.md"),
            format!("[中文](start.zh.md)\n\n{example}"),
        )
        .unwrap();
        fs::write(
            directory.join("start.zh.md"),
            format!(
                "[English](start.md)\n\n{}\n<!-- rsi-example broken -->\n",
                example.replace("ok", "different")
            ),
        )
        .unwrap();
        fs::write(directory.join("orphan.md"), example).unwrap();
        let mut diagnostics = Vec::new();
        validate(root.path(), &mut diagnostics);
        for expected in [
            "English/Chinese pair",
            "duplicate user example id: a",
            "needs id and platform",
            "command examples differ for id `a`",
        ] {
            assert!(
                diagnostics.iter().any(|d| d.message.contains(expected)),
                "{expected}: {diagnostics:?}"
            );
        }
        assert!(
            diagnostics
                .iter()
                .any(|d| d.line == 3 && d.message.contains("examples differ"))
        );
    }

    #[test]
    fn io_errors_are_reported_without_fabricated_comparisons() {
        let errors = validate_pair(b"\xff", b"[English](start.md)");
        assert!(
            errors
                .iter()
                .any(|e| e.contains("could not read user tutorial"))
        );
        assert!(
            !errors
                .iter()
                .any(|e| e.contains("examples differ") || e.contains("link to its translation"))
        );
        let root = tempfile::tempdir().unwrap();
        fs::create_dir_all(root.path().join("docs")).unwrap();
        fs::write(root.path().join("docs/user"), b"not a directory").unwrap();
        let mut diagnostics = Vec::new();
        validate(root.path(), &mut diagnostics);
        assert!(
            diagnostics
                .iter()
                .any(|d| d.message.contains("could not read directory"))
        );
    }

    #[test]
    fn markers_inside_code_are_not_contracts_and_unclosed_fences_are_errors() {
        let mut diagnostics = Vec::new();
        let mut report = |line, message| diagnostics.push((line, message));
        let examples = examples(
            "```html\n<!-- rsi-example fake -->\n```\n\n<!-- rsi-example id=a platform=all -->\n```bash\necho no closing fence\n",
            &mut report,
        );
        assert!(examples.commands.is_empty());
        assert_eq!(diagnostics.len(), 1);
        assert_eq!(diagnostics[0].0, 6);
        assert!(diagnostics[0].1.contains("unterminated"));
    }
}
