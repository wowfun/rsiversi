use super::{Diagnostic, display_path, line_number, markdown_links, parser, resolve_link_target};
use pulldown_cmark::{CodeBlockKind, Event, Tag, TagEnd};
use std::{collections::BTreeMap, fs, path::Path};
mod layout;
use layout::user_paths;

#[derive(Default)]
struct Examples {
    identities: BTreeMap<String, usize>,
    commands: BTreeMap<String, (String, String, String)>,
}

fn has_locale_suffix(path: &Path) -> bool {
    path.file_stem()
        .expect("Markdown file")
        .to_string_lossy()
        .rsplit_once('.')
        .is_some_and(|(_, suffix)| {
            suffix.eq_ignore_ascii_case(layout::LOCALE)
                || ["zh", "zh-Hans"]
                    .iter()
                    .any(|alias| suffix.eq_ignore_ascii_case(alias))
        })
}

pub(super) fn validate(repository: &Path, diagnostics: &mut Vec<Diagnostic>) {
    layout::validate(repository, diagnostics);
    let english_directory = repository.join("docs/user");
    let translated_directory = layout::translation_root(repository);
    let mut paths = user_paths(repository, &english_directory, diagnostics)
        .into_iter()
        .map(|path| (true, path))
        .collect::<Vec<_>>();
    paths.extend(
        user_paths(repository, &translated_directory, diagnostics)
            .into_iter()
            .map(|path| (false, path)),
    );
    let mut documents = BTreeMap::new();
    let mut identities = BTreeMap::new();
    for (english, path) in paths {
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
        if has_locale_suffix(&path) {
            diagnostics.push(Diagnostic::new(
                repository,
                &path,
                1,
                if english {
                    "user translation must use docs/i18n/zh-CN/user/<path>/name.md"
                } else {
                    "user translation filename must omit the locale suffix"
                },
            ));
            continue;
        }
        let (source_directory, paired_directory) = if english {
            (&english_directory, &translated_directory)
        } else {
            (&translated_directory, &english_directory)
        };
        let relative = path.strip_prefix(source_directory).expect("tutorial root");
        let paired = paired_directory.join(relative);
        validate_pair(repository, &path, &paired, &contents, diagnostics);
        if !english {
            layout::localized_links(repository, &path, &paired, &contents, diagnostics);
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
                            display_path(repository, &previous)
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

fn validate_pair(
    repository: &Path,
    path: &Path,
    paired: &Path,
    contents: &str,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let counterpart = display_path(repository, paired);
    if !paired.is_file() {
        diagnostics.push(Diagnostic::new(
            repository,
            path,
            1,
            format!("user tutorial requires its English/Simplified Chinese pair: {counterpart}"),
        ));
    } else if !markdown_links(contents).iter().any(|link| {
        !link.is_image
            && resolve_link_target(repository, path, &link.destination)
                .is_ok_and(|(target, _)| target == paired)
    }) {
        diagnostics.push(Diagnostic::new(
            repository,
            path,
            1,
            format!("user tutorial must link to its translation: {counterpart}"),
        ));
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
        let translated_directory = root.path().join("docs/i18n/zh-CN/user/nested");
        fs::create_dir_all(&directory).unwrap();
        fs::create_dir_all(&translated_directory).unwrap();
        fs::write(directory.join("start.md"), original).unwrap();
        fs::write(translated_directory.join("start.md"), translated).unwrap();
        let mut diagnostics = Vec::new();
        validate(root.path(), &mut diagnostics);
        diagnostics.into_iter().map(|d| d.render()).collect()
    }

    #[test]
    fn commonmark_links_and_fences_are_compared_semantically() {
        let original = "[简体中文](../../i18n/zh-CN/user/nested/./start.md \"简体中文版\")\n\n<!-- rsi-example id=a platform=all -->\n   ~~~~BASH {.x}\n   echo ok\n   ~~~~  \n";
        let translated = "[English][pair]\n\n[pair]: ../../../../user/nested/start.md\n\n<!-- rsi-example id=a platform=all -->\n````bash\necho ok\n````\n";
        assert!(validate_pair(original.as_bytes(), translated.as_bytes()).is_empty());
        for fake in [
            "`[简体中文](../../i18n/zh-CN/user/nested/start.md)`",
            "![简体中文](../../i18n/zh-CN/user/nested/start.md)",
            "```md\n[简体中文](../../i18n/zh-CN/user/nested/start.md)\n```",
        ] {
            let errors = validate_pair(
                fake.as_bytes(),
                b"[English](../../../../user/nested/start.md)",
            );
            assert!(
                errors.iter().any(|e| e.contains("link to its translation")),
                "{fake}: {errors:?}"
            );
        }
    }

    #[test]
    fn missing_sources_and_translations_report_the_exact_nested_counterpart() {
        for (present, missing) in [
            (
                "docs/user/nested/start.md",
                "docs/i18n/zh-CN/user/nested/start.md",
            ),
            (
                "docs/i18n/zh-CN/user/nested/start.md",
                "docs/user/nested/start.md",
            ),
        ] {
            let root = tempfile::tempdir().unwrap();
            let path = root.path().join(present);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, "# Tutorial\n").unwrap();
            let mut diagnostics = Vec::new();
            validate(root.path(), &mut diagnostics);
            assert_eq!(diagnostics.len(), 1, "{diagnostics:?}");
            assert!(diagnostics[0].message.ends_with(missing), "{diagnostics:?}");
        }
    }

    #[test]
    fn translations_require_the_locale_directory_and_locale_neutral_filenames() {
        for (relative, expected) in [
            ("docs/user/start.zh.md", "user translation must use"),
            ("docs/user/start.zh-CN.md", "user translation must use"),
            (
                "docs/i18n/zh-CN/user/start.zh-CN.md",
                "must omit the locale suffix",
            ),
            (
                "docs/i18n/zh-CN/user/start.zh.md",
                "must omit the locale suffix",
            ),
        ] {
            let root = tempfile::tempdir().unwrap();
            let path = root.path().join(relative);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, "# Misplaced translation\n").unwrap();
            let mut diagnostics = Vec::new();
            validate(root.path(), &mut diagnostics);
            assert_eq!(diagnostics.len(), 1, "{diagnostics:?}");
            assert!(diagnostics[0].message.contains(expected), "{diagnostics:?}");
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
        let translated_directory = root.path().join("docs/i18n/zh-CN/user/sub");
        fs::create_dir_all(&directory).unwrap();
        fs::create_dir_all(&translated_directory).unwrap();
        let example = "<!-- rsi-example id=a platform=all -->\n```bash\necho ok\n```\n";
        fs::write(
            directory.join("start.md"),
            format!("[简体中文](../../i18n/zh-CN/user/sub/start.md)\n\n{example}"),
        )
        .unwrap();
        fs::write(
            translated_directory.join("start.md"),
            format!(
                "[English](../../../../user/sub/start.md)\n\n{}\n<!-- rsi-example broken -->\n",
                example.replace("ok", "different")
            ),
        )
        .unwrap();
        fs::write(directory.join("orphan.md"), example).unwrap();
        let mut diagnostics = Vec::new();
        validate(root.path(), &mut diagnostics);
        for expected in [
            "English/Simplified Chinese pair",
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
        let duplicate = diagnostics
            .iter()
            .find(|diagnostic| diagnostic.message.contains("duplicate user example id"))
            .unwrap();
        assert!(
            !duplicate
                .message
                .contains(&root.path().display().to_string())
        );
    }

    #[test]
    fn io_errors_are_reported_without_fabricated_comparisons() {
        let errors = validate_pair(b"\xff", b"[English](../../../../user/nested/start.md)");
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
        for relative in ["docs/user", "docs/i18n/zh-CN/user"] {
            let root = tempfile::tempdir().unwrap();
            let path = root.path().join(relative);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, b"not a directory").unwrap();
            let mut diagnostics = Vec::new();
            validate(root.path(), &mut diagnostics);
            assert!(
                diagnostics
                    .iter()
                    .any(|d| d.message.contains("could not read directory"))
            );
        }
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
