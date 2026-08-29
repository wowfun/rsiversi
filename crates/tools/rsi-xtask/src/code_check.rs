use std::collections::BTreeSet;
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::process::Command;
use std::str::FromStr;

use proc_macro2::{Delimiter, TokenStream, TokenTree};
use serde::Deserialize;

use crate::repository_root;

const CONFIG_PATH: &str = "crates/tools/rsi-xtask/code-check.toml";
const CONFIG_VERSION: u8 = 1;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Config {
    version: u8,
    line_count: LineCountConfig,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct LineCountConfig {
    warning_threshold: usize,
}

#[derive(Debug, Eq, PartialEq)]
struct LineCountWarning {
    path: PathBuf,
    lines: usize,
}

#[derive(Debug, Eq, PartialEq)]
struct LineCountReport {
    scanned_files: usize,
    warnings: Vec<LineCountWarning>,
}

pub fn run(repository: &Path) -> Result<(), String> {
    repository_root::require(repository, "code-check")?;
    let config = read_config(&repository.join(CONFIG_PATH))?;
    let report = line_count_report(repository, config.line_count.warning_threshold)?;

    for warning in &report.warnings {
        eprintln!(
            "warning: code-check line-count: {}: {} effective Rust lines exceed soft warning threshold {}",
            normalized_path(&warning.path),
            warning.lines,
            config.line_count.warning_threshold
        );
    }
    println!(
        "code-check line-count: scanned {} Rust files; {} exceeded warning threshold {}",
        report.scanned_files,
        report.warnings.len(),
        config.line_count.warning_threshold
    );
    Ok(())
}

fn read_config(path: &Path) -> Result<Config, String> {
    let source =
        fs::read_to_string(path).map_err(|error| format!("read {}: {error}", path.display()))?;
    parse_config(&source).map_err(|error| format!("parse {}: {error}", path.display()))
}

fn parse_config(source: &str) -> Result<Config, String> {
    let config = toml::from_str::<Config>(source).map_err(|error| error.to_string())?;
    if config.version != CONFIG_VERSION {
        return Err(format!(
            "unsupported code-check configuration version {}",
            config.version
        ));
    }
    Ok(config)
}

fn line_count_report(
    repository: &Path,
    warning_threshold: usize,
) -> Result<LineCountReport, String> {
    let paths = rust_files(repository)?;
    let mut warnings = Vec::new();
    for path in &paths {
        let lines = effective_lines(&repository.join(path))?;
        if lines > warning_threshold {
            warnings.push(LineCountWarning {
                path: path.clone(),
                lines,
            });
        }
    }
    Ok(LineCountReport {
        scanned_files: paths.len(),
        warnings,
    })
}

fn rust_files(repository: &Path) -> Result<Vec<PathBuf>, String> {
    let output = Command::new("git")
        .args([
            "ls-files",
            "--cached",
            "--others",
            "--exclude-standard",
            "-z",
            "--",
            "*.rs",
        ])
        .current_dir(repository)
        .output()
        .map_err(|error| format!("enumerate repository Rust files with git: {error}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let detail = stderr.trim();
        return if detail.is_empty() {
            Err(format!(
                "enumerate repository Rust files with git: exited with {}",
                output.status
            ))
        } else {
            Err(format!(
                "enumerate repository Rust files with git: {detail}"
            ))
        };
    }

    let mut paths = Vec::new();
    for encoded in output.stdout.split(|byte| *byte == 0) {
        if encoded.is_empty() {
            continue;
        }
        let value = std::str::from_utf8(encoded)
            .map_err(|_| "git returned a non-UTF-8 Rust source path".to_owned())?;
        let relative = PathBuf::from(value);
        if !relative
            .components()
            .all(|part| matches!(part, Component::Normal(_)))
        {
            return Err(format!(
                "git returned a non-normalized Rust source path `{}`",
                relative.display()
            ));
        }
        let metadata = match fs::symlink_metadata(repository.join(&relative)) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => {
                return Err(format!("inspect {}: {error}", relative.display()));
            }
        };
        if metadata.file_type().is_file() {
            paths.push(relative);
        }
    }
    paths.sort();
    paths.dedup();
    Ok(paths)
}

fn effective_lines(path: &Path) -> Result<usize, String> {
    let source =
        fs::read_to_string(path).map_err(|error| format!("read {}: {error}", path.display()))?;
    let tokens = TokenStream::from_str(&source)
        .map_err(|error| format!("tokenize Rust source {}: {error}", path.display()))?;
    let mut occupied = BTreeSet::new();
    let doc_comment_lines = doc_comment_only_lines(&source);
    collect_token_lines(tokens, &doc_comment_lines, &mut occupied);
    Ok(occupied.len())
}

fn collect_token_lines(
    tokens: TokenStream,
    doc_comment_lines: &BTreeSet<usize>,
    occupied: &mut BTreeSet<usize>,
) {
    let tokens = tokens.into_iter().collect::<Vec<_>>();
    let mut index = 0;
    while let Some(token) = tokens.get(index) {
        if let TokenTree::Punct(punct) = token
            && punct.as_char() == '#'
            && doc_comment_lines.contains(&punct.span().start().line)
        {
            let attribute_index = match tokens.get(index + 1) {
                Some(TokenTree::Punct(inner)) if inner.as_char() == '!' => index + 2,
                _ => index + 1,
            };
            if tokens.get(attribute_index).is_some_and(|token| {
                matches!(
                    token,
                    TokenTree::Group(group)
                        if group.delimiter() == Delimiter::Bracket
                            && group.stream().into_iter().next().is_some_and(
                                |token| matches!(token, TokenTree::Ident(ident) if ident == "doc")
                            )
                )
            }) {
                index = attribute_index + 1;
                continue;
            }
        }
        match token {
            TokenTree::Group(group) => {
                mark_span(group.span_open(), occupied);
                collect_token_lines(group.stream(), doc_comment_lines, occupied);
                mark_span(group.span_close(), occupied);
            }
            TokenTree::Ident(token) => mark_span(token.span(), occupied),
            TokenTree::Punct(token) => mark_span(token.span(), occupied),
            TokenTree::Literal(token) => mark_span(token.span(), occupied),
        }
        index += 1;
    }
}

fn doc_comment_only_lines(source: &str) -> BTreeSet<usize> {
    let mut lines = BTreeSet::new();
    let mut block_depth = 0_usize;
    for (offset, line) in source.lines().enumerate() {
        let line_number = offset + 1;
        let trimmed = line.trim_start();
        if block_depth == 0 {
            if trimmed.starts_with("///") || trimmed.starts_with("//!") {
                lines.insert(line_number);
                continue;
            }
            if !(trimmed.starts_with("/**") || trimmed.starts_with("/*!")) {
                continue;
            }
        }
        let remainder = consume_leading_block_comment(trimmed, &mut block_depth);
        if block_depth != 0
            || remainder.trim().is_empty()
            || remainder.trim_start().starts_with("//")
        {
            lines.insert(line_number);
        }
    }
    lines
}

fn consume_leading_block_comment<'a>(line: &'a str, depth: &mut usize) -> &'a str {
    let bytes = line.as_bytes();
    let mut cursor = 0_usize;
    if *depth == 0 && bytes.starts_with(b"/*") {
        *depth = 1;
        cursor = 2;
    }
    while cursor + 1 < bytes.len() && *depth != 0 {
        match &bytes[cursor..cursor + 2] {
            b"/*" => {
                *depth += 1;
                cursor += 2;
            }
            b"*/" => {
                *depth -= 1;
                cursor += 2;
            }
            _ => cursor += 1,
        }
    }
    if *depth == 0 { &line[cursor..] } else { "" }
}

fn mark_span(span: proc_macro2::Span, occupied: &mut BTreeSet<usize>) {
    let start = span.start().line;
    let end = span.end().line.max(start);
    occupied.extend(start..=end);
}

fn normalized_path(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn initialize_git(repository: &Path) {
        let output = Command::new("git")
            .args(["init", "--quiet"])
            .current_dir(repository)
            .output()
            .expect("git should run");
        assert!(
            output.status.success(),
            "git init failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn write(path: &Path, contents: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, contents).unwrap();
    }

    #[test]
    fn config_owns_the_line_count_warning_threshold() {
        let config = parse_config("version = 1\n[line_count]\nwarning_threshold = 1200\n")
            .expect("current configuration should parse");
        assert_eq!(config.line_count.warning_threshold, 1_200);

        let old = "version = 1\nhard_limit = 1200\n[regions]\ncore = 1\n";
        assert!(parse_config(old).is_err(), "old baseline fields must fail");
        let error = parse_config("version = 2\n[line_count]\nwarning_threshold = 1200\n")
            .expect_err("unsupported versions must fail");
        assert!(error.contains("unsupported code-check configuration version 2"));
    }

    #[test]
    fn effective_lines_count_dereferences_but_not_block_comment_stars() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("sample.rs");
        write(
            &path,
            "/*\n * prose\n */\npub fn update(bytes: &mut usize) {\n    *bytes = 1;\n}\n",
        );
        assert_eq!(effective_lines(&path).unwrap(), 3);
    }

    #[test]
    fn non_ascii_block_comments_preserve_the_following_code_boundary() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("sample.rs");
        write(&path, "/* 中文说明 */ pub fn checked() {}\n");
        assert_eq!(effective_lines(&path).unwrap(), 1);
    }

    #[test]
    fn doc_like_text_inside_a_raw_literal_remains_code() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("sample.rs");
        write(&path, "const RAW: &str = r#\"\n/// literal content\n\"#;\n");
        assert_eq!(effective_lines(&path).unwrap(), 3);
    }

    #[test]
    fn discovers_every_non_ignored_regular_rust_file_in_stable_order() {
        let repository = tempfile::tempdir().unwrap();
        initialize_git(repository.path());
        write(
            &repository.path().join(".gitignore"),
            ".local/\ntarget/\nignored.rs\ntracked.rs\n",
        );
        for path in [
            "fixtures/demo/src/main.rs",
            "src/lib.rs",
            "tests/integration.rs",
            "tracked.rs",
            "ignored.rs",
            "target/generated.rs",
            ".local/scratch.rs",
        ] {
            write(&repository.path().join(path), "fn checked() {}\n");
        }
        let output = Command::new("git")
            .args(["add", "--force", "tracked.rs"])
            .current_dir(repository.path())
            .output()
            .unwrap();
        assert!(output.status.success());
        #[cfg(unix)]
        std::os::unix::fs::symlink("src/lib.rs", repository.path().join("linked.rs")).unwrap();

        assert_eq!(
            rust_files(repository.path()).unwrap(),
            [
                "fixtures/demo/src/main.rs",
                "src/lib.rs",
                "tests/integration.rs",
                "tracked.rs",
            ]
            .into_iter()
            .map(PathBuf::from)
            .collect::<Vec<_>>()
        );
    }

    #[test]
    fn counts_test_items_but_not_blank_or_comment_only_lines() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("sample.rs");
        write(
            &path,
            "//! module docs\n// comment\n\n/// item docs\n/** more item docs */\npub fn kept() {\n    let value = 1; // inline\n}\n\n#[cfg(test)]\nmod tests {\n    fn included() {}\n}\n#[cfg(feature = \"test-failpoints\")]\nfn failpoint() {}\n#[cfg(not(feature = \"test-failpoints\"))]\nfn production() {}\n",
        );
        assert_eq!(effective_lines(&path).unwrap(), 11);
    }

    #[test]
    fn reports_only_files_strictly_above_the_threshold() {
        let repository = tempfile::tempdir().unwrap();
        initialize_git(repository.path());
        let line = "const _: () = ();\n";
        write(&repository.path().join("equal.rs"), &line.repeat(1_200));
        write(&repository.path().join("large.rs"), &line.repeat(1_201));

        assert_eq!(
            line_count_report(repository.path(), 1_200).unwrap(),
            LineCountReport {
                scanned_files: 2,
                warnings: vec![LineCountWarning {
                    path: PathBuf::from("large.rs"),
                    lines: 1_201,
                }],
            }
        );
    }
}
