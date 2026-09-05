use std::collections::BTreeSet;
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::process::Command;

use ra_ap_syntax::{
    AstNode, Edition, NodeOrToken, SourceFile, SyntaxKind, SyntaxNode, TextRange, TextSize,
    WalkEvent,
    ast::{self, HasModuleItem, HasName},
};
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
    structure: StructureSummary,
}

#[derive(Debug, Eq, PartialEq)]
struct LineCountReport {
    scanned_files: usize,
    warnings: Vec<LineCountWarning>,
}

#[derive(Debug, Eq, PartialEq)]
struct StructureSummary {
    top_level_items: Vec<SizeMetric>,
    largest_callable: Option<SizeMetric>,
    deepest_control_flow: Option<DepthMetric>,
}

#[derive(Debug, Eq, PartialEq)]
struct SizeMetric {
    label: String,
    line: usize,
    lines: usize,
}

#[derive(Debug, Eq, PartialEq)]
struct DepthMetric {
    label: String,
    line: usize,
    depth: usize,
}

#[derive(Debug, Eq, Ord, PartialEq, PartialOrd)]
struct SourceError {
    path: PathBuf,
    line: Option<usize>,
    column: Option<usize>,
    message: String,
}

#[derive(Debug, Eq, PartialEq)]
struct SourceAnalysis {
    lines: usize,
    structure: StructureSummary,
}

pub fn run(repository: &Path) -> Result<(), String> {
    repository_root::require(repository, "code-check")?;
    let config = read_config(&repository.join(CONFIG_PATH))?;
    let report = line_count_report(repository, config.line_count.warning_threshold)?;

    for warning in &report.warnings {
        let path = normalized_path(&warning.path);
        eprintln!(
            "warning: code-check line-count: {}: {} effective Rust lines exceed soft warning threshold {}",
            path, warning.lines, config.line_count.warning_threshold
        );
        for item in &warning.structure.top_level_items {
            eprintln!(
                "  top-level item: {}:{}: {}: {} effective Rust lines",
                path, item.line, item.label, item.lines
            );
        }
        if let Some(callable) = &warning.structure.largest_callable {
            eprintln!(
                "  largest callable: {}:{}: {}: {} effective Rust lines",
                path, callable.line, callable.label, callable.lines
            );
        }
        if let Some(control_flow) = &warning.structure.deepest_control_flow {
            eprintln!(
                "  deepest control flow: {}:{}: {}: depth {}",
                path, control_flow.line, control_flow.label, control_flow.depth
            );
        }
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
    let mut errors = Vec::new();
    for path in &paths {
        let source = match fs::read_to_string(repository.join(path)) {
            Ok(source) => source,
            Err(error) => {
                errors.push(SourceError {
                    path: path.clone(),
                    line: None,
                    column: None,
                    message: format!("read Rust source: {error}"),
                });
                continue;
            }
        };
        match analyze_source(&source) {
            Ok(analysis) if analysis.lines > warning_threshold => {
                warnings.push(LineCountWarning {
                    path: path.clone(),
                    lines: analysis.lines,
                    structure: analysis.structure,
                });
            }
            Ok(_) => {}
            Err(source_errors) => {
                errors.extend(source_errors.into_iter().map(|error| SourceError {
                    path: path.clone(),
                    line: Some(error.line),
                    column: Some(error.column),
                    message: error.message,
                }));
            }
        }
    }
    if !errors.is_empty() {
        errors.sort();
        let rendered = errors
            .iter()
            .map(render_source_error)
            .collect::<Vec<_>>()
            .join("\n");
        return Err(format!("analyze Rust sources:\n{rendered}"));
    }
    warnings.sort_by(|left, right| {
        right
            .lines
            .cmp(&left.lines)
            .then_with(|| left.path.cmp(&right.path))
    });
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

#[derive(Debug, Eq, PartialEq)]
struct SyntaxError {
    line: usize,
    column: usize,
    message: String,
}

fn analyze_source(source: &str) -> Result<SourceAnalysis, Vec<SyntaxError>> {
    let line_map = LineMap::new(source);
    let parse = SourceFile::parse(source, Edition::CURRENT);
    let errors = parse
        .errors()
        .into_iter()
        .map(|error| {
            let (line, column) = line_map.line_column(error.range().start());
            SyntaxError {
                line,
                column,
                message: error.to_string(),
            }
        })
        .collect::<Vec<_>>();
    if !errors.is_empty() {
        return Err(errors);
    }

    let tree = parse.tree();
    let occupied = occupied_lines(tree.syntax(), &line_map);
    Ok(SourceAnalysis {
        lines: occupied.len(),
        structure: structure_summary(&tree, &line_map, &occupied),
    })
}

fn occupied_lines(root: &SyntaxNode, line_map: &LineMap<'_>) -> BTreeSet<usize> {
    let mut occupied = BTreeSet::new();
    for element in root.descendants_with_tokens() {
        if let NodeOrToken::Token(token) = element
            && !token.kind().is_trivia()
        {
            line_map.mark_lines(token.text_range(), &mut occupied);
        }
    }
    occupied
}

fn structure_summary(
    tree: &SourceFile,
    line_map: &LineMap<'_>,
    occupied: &BTreeSet<usize>,
) -> StructureSummary {
    let mut top_level_items = tree
        .items()
        .map(|item| SizeMetric {
            label: item_label(&item),
            line: line_map.line(item.syntax().text_range().start()),
            lines: line_map.effective_lines(item.syntax().text_range(), occupied),
        })
        .filter(|item| item.lines != 0)
        .collect::<Vec<_>>();
    sort_size_metrics(&mut top_level_items);
    top_level_items.truncate(3);

    let callables = tree
        .syntax()
        .descendants()
        .filter_map(ast::Fn::cast)
        .filter_map(|function| {
            let name = function.name()?;
            let label = format!("fn {}", name.text());
            let line = line_map.line(function.syntax().text_range().start());
            Some((
                function.clone(),
                SizeMetric {
                    label,
                    line,
                    lines: line_map.effective_lines(function.syntax().text_range(), occupied),
                },
            ))
        })
        .collect::<Vec<_>>();

    let mut callable_sizes = callables
        .iter()
        .map(|(_, metric)| SizeMetric {
            label: metric.label.clone(),
            line: metric.line,
            lines: metric.lines,
        })
        .collect::<Vec<_>>();
    sort_size_metrics(&mut callable_sizes);

    let mut control_flows = callables
        .iter()
        .map(|(function, metric)| {
            control_flow_metric(function, &metric.label, metric.line, line_map)
        })
        .collect::<Vec<_>>();
    control_flows.sort_by(|left, right| {
        right
            .depth
            .cmp(&left.depth)
            .then_with(|| left.line.cmp(&right.line))
            .then_with(|| left.label.cmp(&right.label))
    });

    StructureSummary {
        top_level_items,
        largest_callable: callable_sizes.into_iter().next(),
        deepest_control_flow: control_flows.into_iter().next(),
    }
}

fn sort_size_metrics(metrics: &mut [SizeMetric]) {
    metrics.sort_by(|left, right| {
        right
            .lines
            .cmp(&left.lines)
            .then_with(|| left.line.cmp(&right.line))
            .then_with(|| left.label.cmp(&right.label))
    });
}

fn item_label(item: &ast::Item) -> String {
    match item {
        ast::Item::AsmExpr(_) => "asm".into(),
        ast::Item::Const(item) => named_label("const", item.name()),
        ast::Item::Enum(item) => named_label("enum", item.name()),
        ast::Item::ExternBlock(_) => "extern block".into(),
        ast::Item::ExternCrate(item) => item.name_ref().map_or_else(
            || "extern crate".into(),
            |name| format!("extern crate {}", name.text()),
        ),
        ast::Item::Fn(item) => named_label("fn", item.name()),
        ast::Item::Impl(item) => impl_label(item),
        ast::Item::MacroCall(item) => item.path().map_or_else(
            || "macro call".into(),
            |path| format!("macro {}!", compact_text(path.syntax())),
        ),
        ast::Item::MacroDef(item) => named_label("macro", item.name()),
        ast::Item::MacroRules(item) => named_label("macro_rules!", item.name()),
        ast::Item::Module(item) => named_label("mod", item.name()),
        ast::Item::Static(item) => named_label("static", item.name()),
        ast::Item::Struct(item) => named_label("struct", item.name()),
        ast::Item::Trait(item) => named_label("trait", item.name()),
        ast::Item::TypeAlias(item) => named_label("type", item.name()),
        ast::Item::Union(item) => named_label("union", item.name()),
        ast::Item::Use(_) => "use".into(),
    }
}

fn named_label(kind: &str, name: Option<ast::Name>) -> String {
    name.map_or_else(|| kind.into(), |name| format!("{kind} {}", name.text()))
}

fn impl_label(item: &ast::Impl) -> String {
    let self_type = item.self_ty().map(|ty| compact_text(ty.syntax()));
    let trait_type = item.trait_().map(|ty| compact_text(ty.syntax()));
    match (trait_type, self_type) {
        (Some(trait_type), Some(self_type)) => format!("impl {trait_type} for {self_type}"),
        (None, Some(self_type)) => format!("impl {self_type}"),
        _ => "impl".into(),
    }
}

fn compact_text(node: &SyntaxNode) -> String {
    const MAX_CHARS: usize = 80;

    let text = node
        .text()
        .to_string()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    let mut chars = text.chars();
    let prefix = chars.by_ref().take(MAX_CHARS).collect::<String>();
    if chars.next().is_some() {
        format!("{prefix}...")
    } else {
        prefix
    }
}

fn control_flow_metric(
    function: &ast::Fn,
    label: &str,
    function_line: usize,
    line_map: &LineMap<'_>,
) -> DepthMetric {
    let Some(body) = function.body() else {
        return DepthMetric {
            label: label.into(),
            line: function_line,
            depth: 0,
        };
    };

    let mut depth = 0_usize;
    let mut deepest = DepthMetric {
        label: label.into(),
        line: function_line,
        depth: 0,
    };
    let mut skipped_nodes = 0_usize;
    for event in body.syntax().preorder() {
        match event {
            WalkEvent::Enter(_) if skipped_nodes != 0 => skipped_nodes += 1,
            WalkEvent::Leave(_) if skipped_nodes != 0 => skipped_nodes -= 1,
            WalkEvent::Enter(node) if ast::Fn::can_cast(node.kind()) => skipped_nodes = 1,
            WalkEvent::Enter(node) if is_control_flow(&node) => {
                depth += 1;
                if depth > deepest.depth {
                    deepest.depth = depth;
                    deepest.line = line_map.line(node.text_range().start());
                }
            }
            WalkEvent::Leave(node) if is_control_flow(&node) => depth -= 1,
            WalkEvent::Enter(_) | WalkEvent::Leave(_) => {}
        }
    }
    deepest
}

fn is_control_flow(node: &SyntaxNode) -> bool {
    if matches!(
        node.kind(),
        SyntaxKind::IF_EXPR
            | SyntaxKind::MATCH_EXPR
            | SyntaxKind::FOR_EXPR
            | SyntaxKind::WHILE_EXPR
            | SyntaxKind::LOOP_EXPR
            | SyntaxKind::CLOSURE_EXPR
    ) {
        return true;
    }
    ast::BlockExpr::cast(node.clone())
        .is_some_and(|block| block.async_token().is_some() || block.try_block_modifier().is_some())
}

fn render_source_error(error: &SourceError) -> String {
    match (error.line, error.column) {
        (Some(line), Some(column)) => format!(
            "{}:{line}:{column}: {}",
            normalized_path(&error.path),
            error.message
        ),
        _ => format!("{}: {}", normalized_path(&error.path), error.message),
    }
}

struct LineMap<'a> {
    source: &'a str,
    starts: Vec<usize>,
}

impl<'a> LineMap<'a> {
    fn new(source: &'a str) -> Self {
        let mut starts = vec![0];
        starts.extend(
            source
                .as_bytes()
                .iter()
                .enumerate()
                .filter_map(|(offset, byte)| (*byte == b'\n').then_some(offset + 1)),
        );
        Self { source, starts }
    }

    fn line(&self, offset: TextSize) -> usize {
        let offset = text_offset(offset);
        self.starts.partition_point(|start| *start <= offset)
    }

    fn line_column(&self, offset: TextSize) -> (usize, usize) {
        let offset = text_offset(offset);
        let line = self.starts.partition_point(|start| *start <= offset);
        let line_start = self.starts[line.saturating_sub(1)];
        let column = self.source[line_start..offset].chars().count() + 1;
        (line, column)
    }

    fn mark_lines(&self, range: TextRange, occupied: &mut BTreeSet<usize>) {
        let (start, end) = self.lines_for_range(range);
        occupied.extend(start..=end);
    }

    fn effective_lines(&self, range: TextRange, occupied: &BTreeSet<usize>) -> usize {
        let (start, end) = self.lines_for_range(range);
        occupied.range(start..=end).count()
    }

    fn lines_for_range(&self, range: TextRange) -> (usize, usize) {
        let start = text_offset(range.start());
        let end = text_offset(range.end());
        let inclusive_end = end.saturating_sub(usize::from(end > start));
        (
            self.starts
                .partition_point(|line_start| *line_start <= start),
            self.starts
                .partition_point(|line_start| *line_start <= inclusive_end),
        )
    }
}

fn text_offset(offset: TextSize) -> usize {
    usize::try_from(u32::from(offset)).expect("Rust source offsets fit usize")
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
        let source = "/*\n * prose\n */\npub fn update(bytes: &mut usize) {\n    *bytes = 1;\n}\n";
        assert_eq!(analyze_source(source).unwrap().lines, 3);
    }

    #[test]
    fn non_ascii_block_comments_preserve_the_following_code_boundary() {
        assert_eq!(
            analyze_source("/* 中文说明 */ pub fn checked() {}\n")
                .unwrap()
                .lines,
            1
        );
    }

    #[test]
    fn doc_like_text_inside_a_raw_literal_remains_code() {
        assert_eq!(
            analyze_source("const RAW: &str = r#\"\n/// literal content\n\"#;\n")
                .unwrap()
                .lines,
            3
        );
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
        let source = "//! module docs\n// comment\n\n/// item docs\n/** more item docs */\npub fn kept() {\n    let value = 1; // inline\n}\n\n#[cfg(test)]\nmod tests {\n    fn included() {}\n}\n#[cfg(feature = \"test-failpoints\")]\nfn failpoint() {}\n#[cfg(not(feature = \"test-failpoints\"))]\nfn production() {}\n";
        assert_eq!(analyze_source(source).unwrap().lines, 11);
    }

    #[test]
    fn structural_summary_explains_top_level_callable_and_control_flow_hotspots() {
        let source = concat!(
            "fn small() {}\n",
            "\n",
            "impl Widget {\n",
            "    fn largest() {\n",
            "        if ready() {\n",
            "            for item in items() {\n",
            "                match item {\n",
            "                    Some(value) => consume(value),\n",
            "                    None => (),\n",
            "                }\n",
            "            }\n",
            "        }\n",
            "    }\n",
            "\n",
            "    fn shallow() {}\n",
            "}\n",
            "\n",
            "mod tests {\n",
            "    fn case() {\n",
            "        let _ = 1;\n",
            "    }\n",
            "}\n",
        );

        assert_eq!(
            analyze_source(source).unwrap().structure,
            StructureSummary {
                top_level_items: vec![
                    SizeMetric {
                        label: "impl Widget".into(),
                        line: 3,
                        lines: 13,
                    },
                    SizeMetric {
                        label: "mod tests".into(),
                        line: 18,
                        lines: 5,
                    },
                    SizeMetric {
                        label: "fn small".into(),
                        line: 1,
                        lines: 1,
                    },
                ],
                largest_callable: Some(SizeMetric {
                    label: "fn largest".into(),
                    line: 4,
                    lines: 10,
                }),
                deepest_control_flow: Some(DepthMetric {
                    label: "fn largest".into(),
                    line: 7,
                    depth: 3,
                }),
            }
        );
    }

    #[test]
    fn nested_named_functions_have_independent_control_flow_depth() {
        let source = concat!(
            "fn outer() {\n",
            "    if ready() {\n",
            "        fn nested() {\n",
            "            if ready() {\n",
            "                while waiting() {}\n",
            "            }\n",
            "        }\n",
            "    }\n",
            "}\n",
        );

        assert_eq!(
            analyze_source(source)
                .unwrap()
                .structure
                .deepest_control_flow,
            Some(DepthMetric {
                label: "fn nested".into(),
                line: 5,
                depth: 2,
            })
        );
    }

    #[test]
    fn execution_regions_count_but_plain_blocks_and_macro_expansions_do_not() {
        let source = concat!(
            "macro_rules! generated { () => { fn hidden() { loop {} } }; }\n",
            "generated!();\n",
            "fn regions() {\n",
            "    {\n",
            "        let _future = async {\n",
            "            let action = || {\n",
            "                if ready() {}\n",
            "            };\n",
            "        };\n",
            "    }\n",
            "    let _: Result<(), ()> = try {\n",
            "        while waiting() {}\n",
            "    };\n",
            "}\n",
        );

        let structure = analyze_source(source).unwrap().structure;
        assert_eq!(structure.largest_callable.unwrap().label, "fn regions");
        assert_eq!(
            structure.deepest_control_flow,
            Some(DepthMetric {
                label: "fn regions".into(),
                line: 7,
                depth: 3,
            })
        );
    }

    #[test]
    fn files_without_named_functions_only_report_top_level_items() {
        let structure = analyze_source("const VALUE: usize = 1;\n")
            .unwrap()
            .structure;
        assert_eq!(
            structure.top_level_items,
            vec![SizeMetric {
                label: "const VALUE".into(),
                line: 1,
                lines: 1,
            }]
        );
        assert_eq!(structure.largest_callable, None);
        assert_eq!(structure.deepest_control_flow, None);
    }

    #[test]
    fn reports_only_files_above_the_threshold_in_priority_order() {
        let repository = tempfile::tempdir().unwrap();
        initialize_git(repository.path());
        let line = "const _: () = ();\n";
        write(&repository.path().join("equal.rs"), &line.repeat(1_200));
        write(&repository.path().join("a_tie.rs"), &line.repeat(1_201));
        write(&repository.path().join("z_tie.rs"), &line.repeat(1_201));
        write(&repository.path().join("largest.rs"), &line.repeat(1_202));

        let report = line_count_report(repository.path(), 1_200).unwrap();
        assert_eq!(report.scanned_files, 4);
        assert_eq!(
            report
                .warnings
                .iter()
                .map(|warning| (warning.path.as_path(), warning.lines))
                .collect::<Vec<_>>(),
            [
                (Path::new("largest.rs"), 1_202),
                (Path::new("a_tie.rs"), 1_201),
                (Path::new("z_tie.rs"), 1_201),
            ]
        );
        assert!(report.warnings.iter().all(|warning| {
            warning.structure.top_level_items.len() == 3
                && warning.structure.largest_callable.is_none()
                && warning.structure.deepest_control_flow.is_none()
        }));
    }
}
