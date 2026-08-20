use std::collections::BTreeMap;
use std::env;
use std::fs;
use std::path::Path;
use std::process::{Command, ExitCode};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

mod cargo_step;
mod code_health;
mod documentation;
mod repository_root;
mod rsi_agent;
mod rsi_ai;
mod rsi_meta;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Status {
    Proposed,
    Implemented,
    Rejected,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Frontmatter {
    name: String,
    comment: Option<String>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ArchiveManifest {
    version: u8,
    notes: BTreeMap<String, String>,
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("{message}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), String> {
    let arguments = env::args().skip(1).collect::<Vec<_>>();
    match arguments.as_slice() {
        [command] if command == "verify-agent-notes" => {
            let repository = env::current_dir()
                .map_err(|error| format!("could not determine repository root: {error}"))?;
            verify_agent_notes(&repository, false)
                .map_err(|error| format!("verify-agent-notes: {error}"))
        }
        [command, write] if command == "verify-agent-notes" && write == "--write" => {
            let repository = env::current_dir()
                .map_err(|error| format!("could not determine repository root: {error}"))?;
            verify_agent_notes(&repository, true)
                .map_err(|error| format!("verify-agent-notes: {error}"))
        }
        [command] if command == "verify-docs" => {
            let repository = env::current_dir()
                .map_err(|error| format!("could not determine repository root: {error}"))?;
            let mut errors = documentation::verify(&repository);
            if let Err(error) = verify_agent_notes(&repository, false) {
                errors.push(error);
            }
            if errors.is_empty() {
                Ok(())
            } else {
                errors.sort();
                Err(format!("verify-docs:\n{}", errors.join("\n")))
            }
        }
        [product, command] if product == "rsi-meta" && command == "conformance" => {
            let repository = env::current_dir()
                .map_err(|error| format!("could not determine repository root: {error}"))?;
            rsi_meta::run(&repository, rsi_meta::Command::Conformance)
        }
        [product, command] if product == "rsi-agent" && command == "conformance" => {
            let repository = env::current_dir()
                .map_err(|error| format!("could not determine repository root: {error}"))?;
            rsi_agent::run(&repository)
        }
        [product, command] if product == "rsi-ai" && command == "conformance" => {
            let repository = env::current_dir()
                .map_err(|error| format!("could not determine repository root: {error}"))?;
            rsi_ai::run(&repository)
        }
        [product, command] if product == "rsi-meta" && command == "release-demo" => {
            let repository = env::current_dir()
                .map_err(|error| format!("could not determine repository root: {error}"))?;
            rsi_meta::run(&repository, rsi_meta::Command::ReleaseDemo)
        }
        [product, command] if product == "rsi-meta" && command == "code-health" => {
            let repository = env::current_dir()
                .map_err(|error| format!("could not determine repository root: {error}"))?;
            code_health::run(&repository, false)
                .map_err(|error| format!("rsi-meta code-health:\n{error}"))
        }
        [product, command, write]
            if product == "rsi-meta" && command == "code-health" && write == "--write" =>
        {
            let repository = env::current_dir()
                .map_err(|error| format!("could not determine repository root: {error}"))?;
            code_health::run(&repository, true)
                .map_err(|error| format!("rsi-meta code-health:\n{error}"))
        }
        _ => Err(
            "usage: rsi-xtask verify-agent-notes [--write] | rsi-xtask verify-docs | rsi-xtask rsi-ai conformance | rsi-xtask rsi-agent conformance | rsi-xtask rsi-meta <code-health [--write]|conformance|release-demo>"
                .into(),
        ),
    }
}

fn verify_agent_notes(repository: &Path, write: bool) -> Result<(), String> {
    require_repository_root(repository)?;
    for legacy in ["docs/adr", "docs/rfc", "docs/rfcs"] {
        if repository.join(legacy).exists() {
            return Err(format!("legacy decision home `{legacy}` is forbidden"));
        }
    }

    let notes = repository.join(".agents/notes");
    validate_notes_root(&notes)?;
    for (lifecycle, status) in [
        ("proposed", Status::Proposed),
        ("implemented", Status::Implemented),
        ("rejected", Status::Rejected),
    ] {
        let lifecycle_directory = notes.join(lifecycle);
        if !lifecycle_directory.exists() {
            continue;
        }
        for class in active_class_directories(&lifecycle_directory)? {
            validate_class_directory(&class)?;
            for path in note_files(&class)? {
                validate_note_filename(&path)?;
                validate_note_file(&path, status)?;
            }
        }
    }
    verify_archive(repository, &notes, write)?;
    Ok(())
}

fn require_repository_root(repository: &Path) -> Result<(), String> {
    let nested_below_repository = repository.ancestors().skip(1).any(|ancestor| {
        ancestor.join("Cargo.toml").is_file() && ancestor.join(".agents/notes").is_dir()
    });
    if nested_below_repository {
        return Err("command must run from the repository root".to_owned());
    }
    Ok(())
}

fn validate_notes_root(notes: &Path) -> Result<(), String> {
    if !notes.exists() {
        return Ok(());
    }
    let entries = fs::read_dir(notes)
        .map_err(|error| format!("could not read `{}`: {error}", notes.display()))?;
    for entry in entries {
        let entry = entry
            .map_err(|error| format!("could not read entry in `{}`: {error}", notes.display()))?;
        let name = entry
            .file_name()
            .into_string()
            .map_err(|_| format!("non-UTF-8 entry in `{}`", notes.display()))?;
        let kind = entry
            .file_type()
            .map_err(|error| format!("could not inspect `{}`: {error}", entry.path().display()))?;
        let allowed = if kind.is_dir() {
            ["proposed", "implemented", "rejected", "archived"].contains(&name.as_str())
        } else if kind.is_file() {
            ["README.md", "AGENTS.md"].contains(&name.as_str())
        } else {
            false
        };
        if !allowed {
            return Err(format!("unexpected entry `.agents/notes/{name}`"));
        }
    }
    Ok(())
}

fn validate_class_directory(class: &Path) -> Result<(), String> {
    let class_name = class
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| format!("non-UTF-8 Agent Note class at `{}`", class.display()))?;
    if [
        "feature",
        "bug-fix",
        "simplification",
        "architecture",
        "process",
        "testing",
    ]
    .contains(&class_name)
    {
        Ok(())
    } else {
        Err(format!("unknown Agent Note class `{class_name}`"))
    }
}

fn validate_note_file(path: &Path, status: Status) -> Result<(), String> {
    let contents = fs::read_to_string(path)
        .map_err(|error| format!("could not read `{}`: {error}", path.display()))?;
    let (frontmatter, body) = parse_frontmatter(&contents, path)?;
    validate_frontmatter(&frontmatter, path)?;
    validate_sections(body, status, path)
}

fn verify_archive(repository: &Path, notes: &Path, write: bool) -> Result<(), String> {
    let archive = notes.join("archived");
    let baseline = baseline_archive_manifest(repository)?;
    if !archive.exists() {
        if let Some(path) = baseline.keys().next() {
            return Err(format!(
                "archive manifest removed or replaced seal `{path}`"
            ));
        }
        return Ok(());
    }
    let manifest_path = archive.join("manifest.json");
    let manifest_contents = fs::read_to_string(&manifest_path).map_err(|error| {
        format!(
            "could not read archive manifest `{}`: {error}",
            manifest_path.display()
        )
    })?;
    let mut manifest: ArchiveManifest =
        serde_json::from_str(&manifest_contents).map_err(|error| {
            format!(
                "archive manifest `{}` is invalid: {error}",
                manifest_path.display()
            )
        })?;
    if manifest.version != 1 {
        return Err(format!(
            "archive manifest version must be 1, got {}",
            manifest.version
        ));
    }
    for (path, hash) in baseline {
        if manifest.notes.get(&path) != Some(&hash) {
            return Err(format!(
                "archive manifest removed or replaced seal `{path}`"
            ));
        }
    }

    let mut archived = BTreeMap::new();
    for class in archive_class_directories(&archive)? {
        validate_class_directory(&class)?;
        let class_name = class
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| format!("non-UTF-8 Agent Note class at `{}`", class.display()))?;
        for path in note_files(&class)? {
            validate_note_filename(&path)?;
            validate_note_file(&path, Status::Implemented)?;
            let filename = path
                .file_name()
                .and_then(|name| name.to_str())
                .ok_or_else(|| format!("non-UTF-8 Agent Note path at `{}`", path.display()))?;
            let relative = format!("{class_name}/{filename}");
            let contents = fs::read(&path)
                .map_err(|error| format!("could not read `{}`: {error}", path.display()))?;
            archived.insert(relative, format!("{:x}", Sha256::digest(contents)));
        }
    }

    for (path, sealed_hash) in &manifest.notes {
        let Some(hash) = archived.get(path) else {
            return Err(format!("archive manifest references missing note `{path}`"));
        };
        if sealed_hash != hash {
            return Err(format!("archived note `{path}` does not match its seal"));
        }
    }
    let unsealed = archived
        .iter()
        .filter(|(path, _)| !manifest.notes.contains_key(*path))
        .map(|(path, hash)| (path.clone(), hash.clone()))
        .collect::<Vec<_>>();
    if write {
        manifest.notes.extend(unsealed);
        let mut serialized = serde_json::to_string_pretty(&manifest)
            .map_err(|error| format!("could not serialize archive manifest: {error}"))?;
        serialized.push('\n');
        fs::write(&manifest_path, serialized).map_err(|error| {
            format!(
                "could not write archive manifest `{}`: {error}",
                manifest_path.display()
            )
        })?;
    } else if let Some((path, _)) = unsealed.first() {
        return Err(format!("archived note `{path}` is not sealed"));
    }
    Ok(())
}

fn baseline_archive_manifest(repository: &Path) -> Result<BTreeMap<String, String>, String> {
    let requested = env::var("RSI_AGENT_NOTES_BASE").ok();
    let reference = if let Some(reference) = requested {
        if reference.len() == 40 && reference.bytes().all(|byte| byte == b'0') {
            return Ok(BTreeMap::new());
        }
        verify_git_reference(repository, &reference)?;
        Some(reference)
    } else if git(repository, &["rev-parse", "--verify", "HEAD"])
        .is_ok_and(|output| output.status.success())
    {
        Some("HEAD".into())
    } else {
        None
    };
    let Some(reference) = reference else {
        return Ok(BTreeMap::new());
    };

    let object = format!("{reference}:.agents/notes/archived/manifest.json");
    let exists = git(repository, &["cat-file", "-e", &object])?;
    if !exists.status.success() {
        return Ok(BTreeMap::new());
    }
    let output = git(repository, &["show", &object])?;
    if !output.status.success() {
        return Err(format!(
            "could not read archive manifest from Git reference `{reference}`: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    let manifest: ArchiveManifest = serde_json::from_slice(&output.stdout).map_err(|error| {
        format!("archive manifest at Git reference `{reference}` is invalid: {error}")
    })?;
    if manifest.version != 1 {
        return Err(format!(
            "archive manifest at Git reference `{reference}` has unsupported version {}",
            manifest.version
        ));
    }
    Ok(manifest.notes)
}

fn verify_git_reference(repository: &Path, reference: &str) -> Result<(), String> {
    if reference.is_empty() {
        return Err("RSI_AGENT_NOTES_BASE must not be empty".into());
    }
    let object = format!("{reference}^{{commit}}");
    let output = git(repository, &["cat-file", "-e", &object])?;
    if output.status.success() {
        Ok(())
    } else {
        Err(format!(
            "Git reference `{reference}` from RSI_AGENT_NOTES_BASE is unavailable"
        ))
    }
}

fn git(repository: &Path, arguments: &[&str]) -> Result<std::process::Output, String> {
    Command::new("git")
        .arg("-C")
        .arg(repository)
        .args(arguments)
        .output()
        .map_err(|error| format!("could not run Git: {error}"))
}

fn validate_note_filename(path: &Path) -> Result<(), String> {
    let filename = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| format!("non-UTF-8 Agent Note filename at `{}`", path.display()))?;
    let valid = filename.strip_suffix(".md").is_some_and(valid_dated_topic);
    if valid {
        Ok(())
    } else {
        Err(format!("invalid Agent Note filename `{filename}`"))
    }
}

fn valid_dated_topic(stem: &str) -> bool {
    let bytes = stem.as_bytes();
    if bytes.len() < 12
        || bytes[4] != b'-'
        || bytes[7] != b'-'
        || bytes[10] != b'-'
        || !bytes[..10]
            .iter()
            .enumerate()
            .all(|(index, byte)| [4, 7].contains(&index) || byte.is_ascii_digit())
    {
        return false;
    }

    let Ok(year) = stem[..4].parse::<u16>() else {
        return false;
    };
    let Ok(month) = stem[5..7].parse::<u8>() else {
        return false;
    };
    let Ok(day) = stem[8..10].parse::<u8>() else {
        return false;
    };
    if year == 0 || day == 0 || day > days_in_month(year, month) {
        return false;
    }

    let topic = &stem[11..];
    !topic.is_empty()
        && !topic.starts_with('-')
        && !topic.ends_with('-')
        && !topic.contains("--")
        && topic
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
}

const fn days_in_month(year: u16, month: u8) -> u8 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if year.is_multiple_of(400) || (year.is_multiple_of(4) && !year.is_multiple_of(100)) => {
            29
        }
        2 => 28,
        _ => 0,
    }
}

fn validate_frontmatter(frontmatter: &Frontmatter, path: &Path) -> Result<(), String> {
    if frontmatter.name.trim().is_empty() || frontmatter.name.contains(['\n', '\r']) {
        return Err(format!(
            "`{}` frontmatter `name` must be a nonempty single line",
            path.display()
        ));
    }
    if frontmatter
        .comment
        .as_ref()
        .is_some_and(|comment| comment.trim().is_empty() || comment.contains(['\n', '\r']))
    {
        return Err(format!(
            "`{}` frontmatter `comment` must be a nonempty single line when present",
            path.display()
        ));
    }
    Ok(())
}

fn directory_entries(path: &Path) -> Result<Vec<fs::DirEntry>, String> {
    let entries = fs::read_dir(path)
        .map_err(|error| format!("could not read `{}`: {error}", path.display()))?;
    let mut entries = entries
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("could not read entry in `{}`: {error}", path.display()))?;
    entries.sort_by_key(fs::DirEntry::file_name);
    Ok(entries)
}

fn active_class_directories(path: &Path) -> Result<Vec<std::path::PathBuf>, String> {
    let mut directories = Vec::new();
    for entry in directory_entries(path)? {
        let kind = entry
            .file_type()
            .map_err(|error| format!("could not inspect `{}`: {error}", entry.path().display()))?;
        if !kind.is_dir() {
            return Err(format!(
                "`{}` must contain only class directories",
                path.display()
            ));
        }
        directories.push(entry.path());
    }
    Ok(directories)
}

fn archive_class_directories(path: &Path) -> Result<Vec<std::path::PathBuf>, String> {
    let mut directories = Vec::new();
    for entry in directory_entries(path)? {
        let kind = entry
            .file_type()
            .map_err(|error| format!("could not inspect `{}`: {error}", entry.path().display()))?;
        if kind.is_dir() {
            directories.push(entry.path());
            continue;
        }
        let name = entry
            .file_name()
            .into_string()
            .map_err(|_| format!("non-UTF-8 entry in `{}`", path.display()))?;
        if !kind.is_file() || name != "manifest.json" {
            return Err(format!("unexpected archive entry `{name}`"));
        }
    }
    Ok(directories)
}

fn note_files(path: &Path) -> Result<Vec<std::path::PathBuf>, String> {
    let mut files = Vec::new();
    for entry in directory_entries(path)? {
        let kind = entry
            .file_type()
            .map_err(|error| format!("could not inspect `{}`: {error}", entry.path().display()))?;
        if !kind.is_file() {
            return Err(format!(
                "Agent Note class directory `{}` must contain only note files",
                path.display()
            ));
        }
        files.push(entry.path());
    }
    Ok(files)
}

fn parse_frontmatter<'a>(contents: &'a str, path: &Path) -> Result<(Frontmatter, &'a str), String> {
    let remainder = contents
        .strip_prefix("---\n")
        .ok_or_else(|| format!("`{}` must begin with YAML frontmatter", path.display()))?;
    let (yaml, body) = remainder
        .split_once("\n---\n")
        .ok_or_else(|| format!("`{}` has unterminated YAML frontmatter", path.display()))?;
    let frontmatter = yaml_serde::from_str(yaml)
        .map_err(|error| format!("`{}` has invalid frontmatter: {error}", path.display()))?;
    Ok((frontmatter, body))
}

fn validate_sections(body: &str, status: Status, path: &Path) -> Result<(), String> {
    let lines = body.lines().collect::<Vec<_>>();
    let headings = second_level_headings(body);
    if contains_level_one_heading(body) {
        return Err(format!(
            "`{}` must not contain a level-one heading; frontmatter `name` is the title",
            path.display()
        ));
    }
    if headings
        .first()
        .is_none_or(|heading| heading.title != "Problem")
    {
        return Err(format!(
            "`{}` must begin its body with `## Problem`",
            path.display()
        ));
    }
    if status == Status::Implemented {
        for forbidden in ["Proposal", "Plan", "Migration plan", "Acceptance criteria"] {
            if headings.iter().any(|heading| heading.title == forbidden) {
                return Err(format!(
                    "`{}` implemented note may not contain `## {forbidden}`",
                    path.display()
                ));
            }
        }
    }
    let required = match status {
        Status::Proposed => [
            "Problem",
            "Proposal",
            "Alternatives considered",
            "Acceptance criteria",
            "Risks",
        ]
        .as_slice(),
        Status::Implemented => [
            "Problem",
            "Decision",
            "Alternatives considered",
            "Consequences",
        ]
        .as_slice(),
        Status::Rejected => ["Problem", "Proposal", "Alternatives considered"].as_slice(),
    };

    let mut previous = None;
    for title in required {
        if headings
            .iter()
            .filter(|heading| heading.title == *title)
            .count()
            > 1
        {
            return Err(format!(
                "`{}` section `## {title}` appears more than once",
                path.display()
            ));
        }
        let Some(position) = headings.iter().position(|heading| heading.title == *title) else {
            return Err(format!(
                "`{}` is missing required section `## {title}`",
                path.display()
            ));
        };
        if previous.is_some_and(|previous| position <= previous) {
            return Err(format!(
                "`{}` required section `## {title}` is out of order",
                path.display()
            ));
        }
        previous = Some(position);

        let start = headings[position].line + 1;
        let end = headings
            .get(position + 1)
            .map_or(lines.len(), |heading| heading.line);
        let has_content = section_has_content(&lines[start..end]);
        if !has_content {
            return Err(format!(
                "`{}` section `## {title}` must contain content",
                path.display()
            ));
        }
    }
    Ok(())
}

fn section_has_content(lines: &[&str]) -> bool {
    let mut fence = None;
    for line in lines {
        let trimmed = line.trim();
        if update_fence(&mut fence, trimmed) {
            continue;
        }
        if fence.is_none() && !trimmed.is_empty() && !trimmed.starts_with('#') {
            return true;
        }
    }
    false
}

fn contains_level_one_heading(body: &str) -> bool {
    let mut fence = None;
    let mut previous_line_has_text = false;
    for line in body.lines() {
        let trimmed = line.trim_start();
        if update_fence(&mut fence, trimmed) {
            previous_line_has_text = false;
            continue;
        }
        if fence.is_none() {
            if trimmed.starts_with("# ")
                || (previous_line_has_text
                    && !trimmed.is_empty()
                    && trimmed.bytes().all(|byte| byte == b'='))
            {
                return true;
            }
            previous_line_has_text = !trimmed.trim_end().is_empty();
        }
    }
    false
}

struct Heading<'a> {
    title: &'a str,
    line: usize,
}

fn second_level_headings(body: &str) -> Vec<Heading<'_>> {
    let mut fence = None;
    let mut headings = Vec::new();
    for (line_number, line) in body.lines().enumerate() {
        let trimmed = line.trim_start();
        if update_fence(&mut fence, trimmed) {
            continue;
        }
        if fence.is_none()
            && let Some(title) = trimmed.strip_prefix("## ")
        {
            headings.push(Heading {
                title: title.trim_end(),
                line: line_number,
            });
        }
    }
    headings
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct FenceMarker {
    character: u8,
    length: usize,
}

fn fence_marker(line: &str) -> Option<FenceMarker> {
    let marker = *line.as_bytes().first()?;
    let length = line.bytes().take_while(|byte| *byte == marker).count();
    if !matches!(marker, b'`' | b'~') || length < 3 {
        return None;
    }
    Some(FenceMarker {
        character: marker,
        length,
    })
}

fn update_fence(fence: &mut Option<FenceMarker>, line: &str) -> bool {
    let Some(marker) = fence_marker(line) else {
        return false;
    };
    match *fence {
        Some(open) if marker.character == open.character && marker.length >= open.length => {
            *fence = None;
            true
        }
        Some(_) => false,
        None => {
            *fence = Some(marker);
            true
        }
    }
}
