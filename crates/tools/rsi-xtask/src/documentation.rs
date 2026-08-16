use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsStr;
use std::fs;
use std::path::{Component, Path, PathBuf};

use percent_encoding::percent_decode_str;
use pulldown_cmark::{Event, HeadingLevel, Options, Parser, Tag, TagEnd};

const IGNORED_DIRECTORIES: &[&str] = &[".git", ".local", ".references", ".rsi-meta", "target"];
const DOCS_SUBDIRECTORIES: &[&str] = &["cookbook", "postmortem", "subsystems", "user"];
const ROOT_AGENT_WORD_LIMIT: usize = 400;
const SUBTREE_AGENT_WORD_LIMIT: usize = 300;

#[derive(Clone, Copy, Debug)]
struct AgentInstructionBudgetOverride {
    path: &'static str,
    max_words: usize,
    reason: &'static str,
}

const AGENT_INSTRUCTION_BUDGET_OVERRIDES: &[AgentInstructionBudgetOverride] = &[];

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct Diagnostic {
    path: String,
    line: usize,
    message: String,
}

impl Diagnostic {
    fn new(repository: &Path, path: &Path, line: usize, message: impl Into<String>) -> Self {
        Self {
            path: display_path(repository, path),
            line,
            message: message.into(),
        }
    }

    fn render(&self) -> String {
        format!("{}:{}: {}", self.path, self.line, self.message)
    }
}

#[derive(Debug)]
struct MarkdownDocument {
    headings: Vec<MarkdownHeading>,
}

#[derive(Debug)]
struct MarkdownHeading {
    level: HeadingLevel,
    title: String,
    line: usize,
}

#[derive(Debug)]
struct Link {
    destination: String,
    line: usize,
}

pub(crate) fn verify(repository: &Path) -> Vec<String> {
    let mut diagnostics = Vec::new();
    validate_repository_root(repository, &mut diagnostics);
    validate_governance_boundaries(repository, &mut diagnostics);
    validate_agent_instruction_budgets(repository, &mut diagnostics);
    validate_docs_taxonomy(repository, &mut diagnostics);
    validate_package_readmes(repository, &mut diagnostics);
    validate_markdown_links(repository, &mut diagnostics);
    diagnostics.sort();
    diagnostics
        .into_iter()
        .map(|diagnostic| diagnostic.render())
        .collect()
}

fn validate_repository_root(repository: &Path, diagnostics: &mut Vec<Diagnostic>) {
    let manifest = repository.join("Cargo.toml");
    let Ok(contents) = fs::read_to_string(&manifest) else {
        diagnostics.push(Diagnostic::new(
            repository,
            &manifest,
            1,
            "verify-docs must run from the repository root containing Cargo.toml",
        ));
        return;
    };
    let Ok(value) = toml::from_str::<toml::Value>(&contents) else {
        diagnostics.push(Diagnostic::new(
            repository,
            &manifest,
            1,
            "repository Cargo.toml is not valid TOML",
        ));
        return;
    };
    if value.get("workspace").is_none() || value.get("package").is_some() {
        diagnostics.push(Diagnostic::new(
            repository,
            &manifest,
            1,
            "verify-docs must run from the virtual workspace root",
        ));
    }
    let xtask = repository.join("crates/tools/rsi-xtask/Cargo.toml");
    if !xtask.is_file() {
        diagnostics.push(Diagnostic::new(
            repository,
            &xtask,
            1,
            "verify-docs must run from the repository root",
        ));
    }
}

fn validate_governance_boundaries(repository: &Path, diagnostics: &mut Vec<Diagnostic>) {
    for relative in [
        "AGENTS.md",
        "docs/AGENTS.md",
        "crates/AGENTS.md",
        "crates/tools/AGENTS.md",
        "plugins/AGENTS.md",
        "fixtures/AGENTS.md",
        "examples/AGENTS.md",
        "schemas/AGENTS.md",
    ] {
        require_file(
            repository,
            relative,
            "required governance boundary is missing",
            diagnostics,
        );
    }

    for collection in ["plugins", "fixtures", "examples", "schemas"] {
        for child in child_directories(&repository.join(collection), repository, diagnostics) {
            require_path(
                repository,
                &child.join("AGENTS.md"),
                "product namespace must define AGENTS.md",
                diagnostics,
            );
        }
    }

    for product in child_directories(&repository.join("crates"), repository, diagnostics) {
        if product.file_name() == Some(OsStr::new("tools")) {
            continue;
        }
        require_path(
            repository,
            &product.join("AGENTS.md"),
            "product namespace must define AGENTS.md",
            diagnostics,
        );
        require_path(
            repository,
            &product.join("README.md"),
            "product namespace must define README.md",
            diagnostics,
        );
    }

    let merged_product_docs = repository.join("crates/rsi-meta/docs/AGENTS.md");
    if merged_product_docs.exists() {
        diagnostics.push(Diagnostic::new(
            repository,
            &merged_product_docs,
            1,
            "rsi-meta docs governance is merged into crates/rsi-meta/AGENTS.md; the redundant boundary must not be reintroduced",
        ));
    }
}

fn validate_agent_instruction_budgets(repository: &Path, diagnostics: &mut Vec<Diagnostic>) {
    validate_agent_instruction_budgets_with_overrides(
        repository,
        AGENT_INSTRUCTION_BUDGET_OVERRIDES,
        diagnostics,
    );
}

fn validate_agent_instruction_budgets_with_overrides(
    repository: &Path,
    overrides: &[AgentInstructionBudgetOverride],
    diagnostics: &mut Vec<Diagnostic>,
) {
    let (active_paths, word_counts) = agent_instruction_word_counts(repository, diagnostics);
    let applicable_overrides = validate_agent_instruction_budget_overrides(
        repository,
        overrides,
        &active_paths,
        &word_counts,
        diagnostics,
    );

    for (relative, word_count) in word_counts {
        let default_limit = default_agent_word_limit(&relative);
        let limit = applicable_overrides
            .get(&relative)
            .copied()
            .unwrap_or(default_limit);
        if word_count > limit {
            diagnostics.push(Diagnostic::new(
                repository,
                &repository.join(&relative),
                1,
                format!(
                    "AGENTS.md contains {word_count} words, exceeding its {limit}-word limit; move non-standing content to its authoritative document, remove duplication or condense, then split rules into a real descendant AGENTS.md; a reasoned path override is the last resort"
                ),
            ));
        }
    }
}

fn agent_instruction_word_counts(
    repository: &Path,
    diagnostics: &mut Vec<Diagnostic>,
) -> (BTreeSet<String>, BTreeMap<String, usize>) {
    let agent_files = files_named(repository, "AGENTS.md", true, diagnostics);
    let active_paths = agent_files
        .iter()
        .map(|path| display_path(repository, path))
        .collect::<BTreeSet<_>>();
    let mut word_counts = BTreeMap::new();

    for path in agent_files {
        match fs::read_to_string(&path) {
            Ok(contents) => {
                word_counts.insert(
                    display_path(repository, &path),
                    contents.split_whitespace().count(),
                );
            }
            Err(error) => diagnostics.push(Diagnostic::new(
                repository,
                &path,
                1,
                format!("could not read AGENTS.md for word-budget validation: {error}"),
            )),
        }
    }

    (active_paths, word_counts)
}

fn validate_agent_instruction_budget_overrides(
    repository: &Path,
    overrides: &[AgentInstructionBudgetOverride],
    active_paths: &BTreeSet<String>,
    word_counts: &BTreeMap<String, usize>,
    diagnostics: &mut Vec<Diagnostic>,
) -> BTreeMap<String, usize> {
    let mut seen_overrides = BTreeSet::new();
    let mut applicable_overrides = BTreeMap::new();
    for budget_override in overrides {
        let duplicate = !seen_overrides.insert(budget_override.path);
        let details_valid = validate_agent_instruction_budget_override(
            repository,
            budget_override,
            active_paths,
            word_counts,
            diagnostics,
        );
        if duplicate {
            let path = if is_normalized_agent_path(budget_override.path) {
                repository.join(budget_override.path)
            } else {
                repository.join("AGENTS.md")
            };
            diagnostics.push(Diagnostic::new(
                repository,
                &path,
                1,
                "AGENTS.md budget override path is duplicated",
            ));
        }
        if !duplicate && details_valid {
            applicable_overrides.insert(budget_override.path.to_owned(), budget_override.max_words);
        }
    }
    applicable_overrides
}

fn validate_agent_instruction_budget_override(
    repository: &Path,
    budget_override: &AgentInstructionBudgetOverride,
    active_paths: &BTreeSet<String>,
    word_counts: &BTreeMap<String, usize>,
    diagnostics: &mut Vec<Diagnostic>,
) -> bool {
    if !is_normalized_agent_path(budget_override.path) {
        diagnostics.push(Diagnostic::new(
            repository,
            &repository.join("AGENTS.md"),
            1,
            format!(
                "AGENTS.md budget override path `{}` must be a normalized repository-relative AGENTS.md path",
                budget_override.path
            ),
        ));
        return false;
    }

    let path = repository.join(budget_override.path);
    let default_limit = default_agent_word_limit(budget_override.path);
    let mut valid = true;
    if budget_override.reason.trim().is_empty() {
        diagnostics.push(Diagnostic::new(
            repository,
            &path,
            1,
            "AGENTS.md budget override must have a nonempty reason",
        ));
        valid = false;
    }
    if budget_override.max_words <= default_limit {
        diagnostics.push(Diagnostic::new(
            repository,
            &path,
            1,
            format!("AGENTS.md budget override must exceed the default {default_limit}-word limit"),
        ));
        valid = false;
    }
    if !active_paths.contains(budget_override.path) {
        diagnostics.push(Diagnostic::new(
            repository,
            &path,
            1,
            "AGENTS.md budget override does not name an active instruction file",
        ));
        valid = false;
    } else if word_counts
        .get(budget_override.path)
        .is_some_and(|word_count| *word_count <= default_limit)
    {
        diagnostics.push(Diagnostic::new(
            repository,
            &path,
            1,
            format!(
                "AGENTS.md budget override is stale because the file is within the default {default_limit}-word limit"
            ),
        ));
        valid = false;
    }

    valid
}

fn default_agent_word_limit(path: &str) -> usize {
    if path == "AGENTS.md" {
        ROOT_AGENT_WORD_LIMIT
    } else {
        SUBTREE_AGENT_WORD_LIMIT
    }
}

fn is_normalized_agent_path(path: &str) -> bool {
    let path = Path::new(path);
    !path.as_os_str().is_empty()
        && !path.is_absolute()
        && !path.to_string_lossy().contains('\\')
        && path.file_name() == Some(OsStr::new("AGENTS.md"))
        && path
            .components()
            .all(|component| matches!(component, Component::Normal(_)))
}

fn validate_docs_taxonomy(repository: &Path, diagnostics: &mut Vec<Diagnostic>) {
    let mut doc_roots = vec![repository.join("docs")];
    for product in child_directories(&repository.join("crates"), repository, diagnostics) {
        let docs = product.join("docs");
        if docs.is_dir() {
            doc_roots.push(docs);
        }
    }

    for docs in doc_roots {
        let Ok(entries) = sorted_entries(&docs) else {
            continue;
        };
        for entry in entries {
            let Ok(kind) = entry.file_type() else {
                diagnostics.push(Diagnostic::new(
                    repository,
                    &entry.path(),
                    1,
                    "could not inspect docs entry",
                ));
                continue;
            };
            let name = entry.file_name();
            if kind.is_dir()
                && !IGNORED_DIRECTORIES.contains(&name.to_string_lossy().as_ref())
                && !DOCS_SUBDIRECTORIES.contains(&name.to_string_lossy().as_ref())
            {
                diagnostics.push(Diagnostic::new(
                    repository,
                    &entry.path(),
                    1,
                    format!(
                        "unsupported docs taxonomy directory `{}`",
                        name.to_string_lossy()
                    ),
                ));
            } else if kind.is_file() && entry.path().extension() != Some(OsStr::new("md")) {
                diagnostics.push(Diagnostic::new(
                    repository,
                    &entry.path(),
                    1,
                    "docs roots may contain only Markdown files and allowed taxonomy directories",
                ));
            }
        }

        for legacy in ["adr", "rfc", "rfcs", "specs"] {
            let path = docs.join(legacy);
            if path.exists() {
                diagnostics.push(Diagnostic::new(
                    repository,
                    &path,
                    1,
                    "legacy decision or specification home is forbidden",
                ));
            }
        }
    }
}

fn validate_package_readmes(repository: &Path, diagnostics: &mut Vec<Diagnostic>) {
    for manifest in files_named(repository, "Cargo.toml", false, diagnostics) {
        if manifest == repository.join("Cargo.toml") {
            continue;
        }
        let contents = match fs::read_to_string(&manifest) {
            Ok(contents) => contents,
            Err(error) => {
                diagnostics.push(Diagnostic::new(
                    repository,
                    &manifest,
                    1,
                    format!("could not read Cargo manifest: {error}"),
                ));
                continue;
            }
        };
        let value = match toml::from_str::<toml::Value>(&contents) {
            Ok(value) => value,
            Err(error) => {
                diagnostics.push(Diagnostic::new(
                    repository,
                    &manifest,
                    1,
                    format!("invalid Cargo manifest: {error}"),
                ));
                continue;
            }
        };
        let Some(package_table) = value.get("package").and_then(toml::Value::as_table) else {
            continue;
        };
        let Some(package_name) = package_table.get("name").and_then(toml::Value::as_str) else {
            diagnostics.push(Diagnostic::new(
                repository,
                &manifest,
                1,
                "Cargo package manifest must define a string `package.name`",
            ));
            continue;
        };
        let package = manifest.parent().expect("Cargo.toml has a parent");
        if !is_recognized_package_path(repository, package) {
            diagnostics.push(Diagnostic::new(
                repository,
                &manifest,
                1,
                "Cargo package path is not a recognized documentation class",
            ));
            continue;
        }
        validate_package_readme(repository, package, package_name, diagnostics);
    }
}

fn is_recognized_package_path(repository: &Path, package: &Path) -> bool {
    let Ok(relative) = package.strip_prefix(repository) else {
        return false;
    };
    let components = relative.components().collect::<Vec<_>>();
    match components.as_slice() {
        [
            Component::Normal(first),
            Component::Normal(second),
            Component::Normal(_),
        ] if *first == OsStr::new("crates")
            && (*second == OsStr::new("rsi-meta") || *second == OsStr::new("tools")) =>
        {
            true
        }
        [
            Component::Normal(first),
            Component::Normal(second),
            Component::Normal(_),
        ] if (*first == OsStr::new("plugins") || *first == OsStr::new("fixtures"))
            && *second == OsStr::new("rsi-meta") =>
        {
            true
        }
        _ => false,
    }
}

fn validate_package_readme(
    repository: &Path,
    package: &Path,
    package_name: &str,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let readme = package.join("README.md");
    let contents = match fs::read_to_string(&readme) {
        Ok(contents) => contents,
        Err(error) => {
            diagnostics.push(Diagnostic::new(
                repository,
                &readme,
                1,
                format!("Cargo package must have a sibling README.md: {error}"),
            ));
            return;
        }
    };
    let document = parse_markdown(&contents);
    let level_one = document
        .headings
        .iter()
        .filter(|heading| heading.level == HeadingLevel::H1)
        .collect::<Vec<_>>();
    match level_one.as_slice() {
        [] => diagnostics.push(Diagnostic::new(
            repository,
            &readme,
            1,
            format!("package README must contain `# {package_name}`"),
        )),
        [heading] if heading.title != package_name => diagnostics.push(Diagnostic::new(
            repository,
            &readme,
            heading.line,
            format!("package README heading must match Cargo package name `{package_name}`"),
        )),
        [.., duplicate] if level_one.len() > 1 => diagnostics.push(Diagnostic::new(
            repository,
            &readme,
            duplicate.line,
            "package README must contain exactly one level-one heading",
        )),
        [_] => {}
        _ => unreachable!("level-one heading cases are exhaustive"),
    }
    if !has_prose_paragraph(&contents) {
        diagnostics.push(Diagnostic::new(
            repository,
            &readme,
            1,
            "package README must contain a nonempty prose paragraph",
        ));
    }
}

fn has_prose_paragraph(contents: &str) -> bool {
    let mut in_paragraph = false;
    for event in parser(contents) {
        match event {
            Event::Start(Tag::Paragraph) => in_paragraph = true,
            Event::End(TagEnd::Paragraph) => in_paragraph = false,
            Event::Text(text) if in_paragraph && !text.trim().is_empty() => return true,
            _ => {}
        }
    }
    false
}

fn validate_markdown_links(repository: &Path, diagnostics: &mut Vec<Diagnostic>) {
    let markdown = files_named(repository, ".md", true, diagnostics);
    let mut documents = BTreeMap::new();
    let mut anchors = BTreeMap::new();
    for path in &markdown {
        let contents = match fs::read_to_string(path) {
            Ok(contents) => contents,
            Err(error) => {
                diagnostics.push(Diagnostic::new(
                    repository,
                    path,
                    1,
                    format!("could not read Markdown: {error}"),
                ));
                continue;
            }
        };
        let document = parse_markdown(&contents);
        anchors.insert(path.clone(), heading_anchors(&document.headings));
        documents.insert(path.clone(), (contents, document));
    }

    for (source, (contents, _)) in &documents {
        for link in markdown_links(contents) {
            validate_link(repository, source, &link, &anchors, diagnostics);
        }
    }
}

fn validate_link(
    repository: &Path,
    source: &Path,
    link: &Link,
    anchors: &BTreeMap<PathBuf, BTreeSet<String>>,
    diagnostics: &mut Vec<Diagnostic>,
) {
    if is_external_destination(&link.destination) {
        return;
    }
    let (target, fragment) = match resolve_link_target(repository, source, &link.destination) {
        Ok(target) => target,
        Err(error) => {
            diagnostics.push(Diagnostic::new(repository, source, link.line, error));
            return;
        }
    };

    let Some(fragment) = fragment else {
        return;
    };
    if target.extension() != Some(OsStr::new("md")) {
        return;
    }
    let decoded_fragment = match decode_url_component(&fragment) {
        Ok(fragment) => fragment,
        Err(error) => {
            diagnostics.push(Diagnostic::new(repository, source, link.line, error));
            return;
        }
    };
    if decoded_fragment.is_empty() {
        return;
    }
    if !anchors
        .get(&target)
        .is_some_and(|target_anchors| target_anchors.contains(&decoded_fragment))
    {
        diagnostics.push(Diagnostic::new(
            repository,
            source,
            link.line,
            format!("Markdown heading fragment `#{decoded_fragment}` does not exist"),
        ));
    }
}

fn resolve_link_target(
    repository: &Path,
    source: &Path,
    destination: &str,
) -> Result<(PathBuf, Option<String>), String> {
    let (without_fragment, fragment) = destination
        .split_once('#')
        .map_or((destination, None), |(path, fragment)| {
            (path, Some(fragment))
        });
    let path_part = without_fragment
        .split_once('?')
        .map_or(without_fragment, |(path, _)| path);
    let decoded_path = decode_url_component(path_part)?;
    let relative = if decoded_path.is_empty() {
        source
            .strip_prefix(repository)
            .map(Path::to_path_buf)
            .map_err(|_| "Markdown source is outside the repository".to_owned())?
    } else {
        resolve_relative(repository, source, &decoded_path)?
    };
    if relative.starts_with(Path::new(".agents/notes/archived")) {
        return Err("active documentation may not link to an archived Agent Note".into());
    }
    let target = exact_case_target(repository, &relative)?;
    let canonical_repository = repository
        .canonicalize()
        .map_err(|error| format!("could not canonicalize repository root: {error}"))?;
    let canonical_target = target
        .canonicalize()
        .map_err(|error| format!("link target does not exist: {error}"))?;
    if !canonical_target.starts_with(&canonical_repository) {
        return Err("link target escapes the repository through a symlink".into());
    }
    Ok((target, fragment.map(str::to_owned)))
}

fn parse_markdown(contents: &str) -> MarkdownDocument {
    let mut headings = Vec::new();
    let mut current: Option<(HeadingLevel, usize, String)> = None;
    for (event, range) in parser(contents).into_offset_iter() {
        match event {
            Event::Start(Tag::Heading { level, .. }) => {
                current = Some((level, line_number(contents, range.start), String::new()));
            }
            Event::Text(text) | Event::Code(text) if current.is_some() => {
                current.as_mut().expect("checked current").2.push_str(&text);
            }
            Event::SoftBreak | Event::HardBreak if current.is_some() => {
                current.as_mut().expect("checked current").2.push(' ');
            }
            Event::End(TagEnd::Heading(level)) => {
                if let Some((start_level, line, title)) = current.take()
                    && start_level == level
                {
                    headings.push(MarkdownHeading {
                        level,
                        title: title.trim().to_owned(),
                        line,
                    });
                }
            }
            _ => {}
        }
    }
    MarkdownDocument { headings }
}

fn markdown_links(contents: &str) -> Vec<Link> {
    parser(contents)
        .into_offset_iter()
        .filter_map(|(event, range)| match event {
            Event::Start(Tag::Link { dest_url, .. } | Tag::Image { dest_url, .. }) => Some(Link {
                destination: dest_url.into_string(),
                line: line_number(contents, range.start),
            }),
            _ => None,
        })
        .collect()
}

fn parser(contents: &str) -> Parser<'_> {
    let mut options = Options::empty();
    options.insert(Options::ENABLE_FOOTNOTES);
    options.insert(Options::ENABLE_STRIKETHROUGH);
    options.insert(Options::ENABLE_TABLES);
    options.insert(Options::ENABLE_TASKLISTS);
    Parser::new_ext(contents, options)
}

fn heading_anchors(headings: &[MarkdownHeading]) -> BTreeSet<String> {
    let mut occurrences = BTreeMap::<String, usize>::new();
    let mut anchors = BTreeSet::new();
    for heading in headings {
        let base = github_slug(&heading.title);
        let occurrence = occurrences.entry(base.clone()).or_default();
        let anchor = if *occurrence == 0 {
            base
        } else {
            format!("{base}-{occurrence}")
        };
        *occurrence += 1;
        anchors.insert(anchor);
    }
    anchors
}

fn github_slug(title: &str) -> String {
    let mut slug = String::new();
    for character in title.chars().flat_map(char::to_lowercase) {
        if character.is_alphanumeric() || matches!(character, '-' | '_') {
            slug.push(character);
        } else if character.is_whitespace() {
            slug.push('-');
        }
    }
    slug
}

fn is_external_destination(destination: &str) -> bool {
    if destination.starts_with("//") {
        return true;
    }
    let before_delimiter = destination
        .split(['/', '#', '?'])
        .next()
        .unwrap_or(destination);
    before_delimiter.contains(':')
}

fn decode_url_component(component: &str) -> Result<String, String> {
    percent_decode_str(component)
        .decode_utf8()
        .map(std::borrow::Cow::into_owned)
        .map_err(|_| format!("URL component `{component}` is not valid UTF-8"))
}

fn resolve_relative(
    repository: &Path,
    source: &Path,
    destination: &str,
) -> Result<PathBuf, String> {
    let mut components = if destination.starts_with('/') {
        Vec::new()
    } else {
        source
            .parent()
            .and_then(|parent| parent.strip_prefix(repository).ok())
            .ok_or_else(|| "Markdown source is outside the repository".to_owned())?
            .components()
            .filter_map(|component| match component {
                Component::Normal(value) => Some(value.to_os_string()),
                _ => None,
            })
            .collect::<Vec<_>>()
    };
    let destination = destination.strip_prefix('/').unwrap_or(destination);
    for component in Path::new(destination).components() {
        match component {
            Component::CurDir => {}
            Component::Normal(value) => components.push(value.to_os_string()),
            Component::ParentDir => {
                if components.pop().is_none() {
                    return Err("link target escapes the repository".into());
                }
            }
            Component::Prefix(_) | Component::RootDir => {
                return Err("link target is not a repository-relative path".into());
            }
        }
    }
    Ok(components.into_iter().collect())
}

fn exact_case_target(repository: &Path, relative: &Path) -> Result<PathBuf, String> {
    let mut current = repository.to_path_buf();
    for component in relative.components() {
        let Component::Normal(expected) = component else {
            return Err("link target is not normalized".into());
        };
        let entries = fs::read_dir(&current)
            .map_err(|_| format!("link target `{}` does not exist", relative.display()))?;
        let mut case_variant = None;
        let mut found = false;
        for entry in entries.flatten() {
            let actual = entry.file_name();
            if actual == expected {
                found = true;
                break;
            }
            if actual
                .to_string_lossy()
                .eq_ignore_ascii_case(&expected.to_string_lossy())
            {
                case_variant = Some(actual);
            }
        }
        if !found {
            if let Some(actual) = case_variant {
                return Err(format!(
                    "link path has incorrect case: `{}` should be `{}`",
                    expected.to_string_lossy(),
                    actual.to_string_lossy()
                ));
            }
            return Err(format!(
                "link target `{}` does not exist",
                relative.display()
            ));
        }
        current.push(expected);
    }
    Ok(current)
}

fn require_file(
    repository: &Path,
    relative: &str,
    message: &str,
    diagnostics: &mut Vec<Diagnostic>,
) {
    require_path(repository, &repository.join(relative), message, diagnostics);
}

fn require_path(repository: &Path, path: &Path, message: &str, diagnostics: &mut Vec<Diagnostic>) {
    if !path.is_file() {
        diagnostics.push(Diagnostic::new(repository, path, 1, message));
    }
}

fn child_directories(
    path: &Path,
    repository: &Path,
    diagnostics: &mut Vec<Diagnostic>,
) -> Vec<PathBuf> {
    let entries = match sorted_entries(path) {
        Ok(entries) => entries,
        Err(error) => {
            diagnostics.push(Diagnostic::new(repository, path, 1, error));
            return Vec::new();
        }
    };
    entries
        .into_iter()
        .filter_map(|entry| {
            entry
                .file_type()
                .ok()
                .filter(fs::FileType::is_dir)
                .map(|_| entry.path())
        })
        .filter(|entry| {
            !entry
                .file_name()
                .is_some_and(|name| IGNORED_DIRECTORIES.contains(&name.to_string_lossy().as_ref()))
        })
        .collect()
}

fn files_named(
    repository: &Path,
    name_or_extension: &str,
    skip_archive: bool,
    diagnostics: &mut Vec<Diagnostic>,
) -> Vec<PathBuf> {
    let mut files = Vec::new();
    visit_files(
        repository,
        repository,
        name_or_extension,
        skip_archive,
        &mut files,
        diagnostics,
    );
    files.sort();
    files
}

fn visit_files(
    repository: &Path,
    directory: &Path,
    name_or_extension: &str,
    skip_archive: bool,
    files: &mut Vec<PathBuf>,
    diagnostics: &mut Vec<Diagnostic>,
) {
    if directory == repository.join(".agents/skills")
        || (skip_archive && directory == repository.join(".agents/notes/archived"))
    {
        return;
    }
    let entries = match sorted_entries(directory) {
        Ok(entries) => entries,
        Err(error) => {
            diagnostics.push(Diagnostic::new(repository, directory, 1, error));
            return;
        }
    };
    for entry in entries {
        let path = entry.path();
        let kind = match entry.file_type() {
            Ok(kind) => kind,
            Err(error) => {
                diagnostics.push(Diagnostic::new(
                    repository,
                    &path,
                    1,
                    format!("could not inspect entry: {error}"),
                ));
                continue;
            }
        };
        if kind.is_dir() {
            if !IGNORED_DIRECTORIES.contains(&entry.file_name().to_string_lossy().as_ref()) {
                visit_files(
                    repository,
                    &path,
                    name_or_extension,
                    skip_archive,
                    files,
                    diagnostics,
                );
            }
        } else if kind.is_file()
            && (entry.file_name() == name_or_extension
                || (name_or_extension == ".md" && path.extension() == Some(OsStr::new("md"))))
        {
            files.push(path);
        }
    }
}

fn sorted_entries(path: &Path) -> Result<Vec<fs::DirEntry>, String> {
    let mut entries = fs::read_dir(path)
        .map_err(|error| format!("could not read directory: {error}"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("could not read directory entry: {error}"))?;
    entries.sort_by_key(fs::DirEntry::file_name);
    Ok(entries)
}

fn line_number(contents: &str, offset: usize) -> usize {
    contents[..offset]
        .bytes()
        .filter(|byte| *byte == b'\n')
        .count()
        + 1
}

fn display_path(repository: &Path, path: &Path) -> String {
    path.strip_prefix(repository)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

#[cfg(test)]
mod tests {
    use super::{
        AgentInstructionBudgetOverride, Diagnostic, github_slug, heading_anchors, parse_markdown,
        resolve_relative, validate_agent_instruction_budgets_with_overrides,
    };
    use std::fs;
    use std::path::Path;
    use tempfile::TempDir;

    fn words(count: usize) -> String {
        (0..count).map(|_| "word").collect::<Vec<_>>().join(" ")
    }

    fn budget_diagnostics(
        files: &[(&str, usize)],
        overrides: &[AgentInstructionBudgetOverride],
    ) -> Vec<String> {
        let repository = TempDir::new().expect("temporary repository");
        for (relative, word_count) in files {
            let path = repository.path().join(relative);
            fs::create_dir_all(path.parent().expect("instruction file parent"))
                .expect("create instruction directory");
            fs::write(path, words(*word_count)).expect("write instruction file");
        }
        let mut diagnostics = Vec::<Diagnostic>::new();
        validate_agent_instruction_budgets_with_overrides(
            repository.path(),
            overrides,
            &mut diagnostics,
        );
        diagnostics.sort();
        diagnostics
            .into_iter()
            .map(|diagnostic| diagnostic.render())
            .collect()
    }

    #[test]
    fn github_slugs_preserve_unicode_and_number_duplicates() {
        let document = parse_markdown("## Über view\n\n## Über view\n\n## 中文 API\n");
        let anchors = heading_anchors(&document.headings);
        assert!(anchors.contains("über-view"));
        assert!(anchors.contains("über-view-1"));
        assert!(anchors.contains("中文-api"));
        assert_eq!(github_slug("Cursor/retry: rules!"), "cursorretry-rules");
    }

    #[test]
    fn relative_paths_cannot_escape_the_repository() {
        let repository = Path::new("/repo");
        let source = Path::new("/repo/docs/page.md");
        assert_eq!(
            resolve_relative(repository, source, "../README.md").expect("inside repository"),
            Path::new("README.md")
        );
        assert!(resolve_relative(repository, source, "../../outside.md").is_err());
    }

    #[test]
    fn parser_recognizes_setext_headings_and_ignores_fenced_headings() {
        let document = parse_markdown("Title\n-----\n\n```md\n## Not a heading\n```\n");
        assert_eq!(document.headings.len(), 1);
        assert_eq!(document.headings[0].title, "Title");
    }

    #[test]
    fn a_reasoned_override_raises_one_active_file_limit() {
        let diagnostics = budget_diagnostics(
            &[
                ("nested/AGENTS.md", 301),
                (".agents/notes/archived/AGENTS.md", 1_000),
            ],
            &[AgentInstructionBudgetOverride {
                path: "nested/AGENTS.md",
                max_words: 302,
                reason: "The cohesive standing rules cannot move to a narrower scope.",
            }],
        );

        assert!(diagnostics.is_empty(), "{diagnostics:?}");
    }

    #[test]
    fn invalid_and_stale_overrides_are_rejected() {
        let diagnostics = budget_diagnostics(
            &[
                ("blank/AGENTS.md", 301),
                ("duplicate/AGENTS.md", 301),
                ("low/AGENTS.md", 301),
                ("stale/AGENTS.md", 300),
            ],
            &[
                AgentInstructionBudgetOverride {
                    path: "blank/AGENTS.md",
                    max_words: 302,
                    reason: "   ",
                },
                AgentInstructionBudgetOverride {
                    path: "duplicate/AGENTS.md",
                    max_words: 302,
                    reason: "First entry.",
                },
                AgentInstructionBudgetOverride {
                    path: "duplicate/AGENTS.md",
                    max_words: 303,
                    reason: "Second entry.",
                },
                AgentInstructionBudgetOverride {
                    path: "low/AGENTS.md",
                    max_words: 300,
                    reason: "Not actually a higher ceiling.",
                },
                AgentInstructionBudgetOverride {
                    path: "stale/AGENTS.md",
                    max_words: 301,
                    reason: "No longer needed.",
                },
                AgentInstructionBudgetOverride {
                    path: "missing/AGENTS.md",
                    max_words: 301,
                    reason: "The file does not exist.",
                },
                AgentInstructionBudgetOverride {
                    path: "../AGENTS.md",
                    max_words: 401,
                    reason: "The path is not normalized.",
                },
            ],
        );
        let errors = diagnostics.join("\n");

        assert!(errors.contains("budget override must have a nonempty reason"));
        assert!(errors.contains("budget override path is duplicated"));
        assert!(errors.contains("budget override must exceed the default 300-word limit"));
        assert!(errors.contains("budget override is stale"));
        assert!(errors.contains("does not name an active instruction file"));
        assert!(errors.contains("must be a normalized repository-relative AGENTS.md path"));
    }

    #[test]
    fn an_override_remains_a_hard_limit() {
        let diagnostics = budget_diagnostics(
            &[("nested/AGENTS.md", 303)],
            &[AgentInstructionBudgetOverride {
                path: "nested/AGENTS.md",
                max_words: 302,
                reason: "A narrow temporary exception.",
            }],
        );

        assert_eq!(diagnostics.len(), 1);
        assert!(diagnostics[0].contains("303 words, exceeding its 302-word limit"));
    }
}
