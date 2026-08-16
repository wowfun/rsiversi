use std::fs;
use std::path::Path;
use std::process::{Command, Output};

use tempfile::TempDir;

fn run_verify(repository: &Path, arguments: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_rsi-xtask"))
        .current_dir(repository)
        .env_remove("RSI_AGENT_NOTES_BASE")
        .arg("verify-agent-notes")
        .args(arguments)
        .output()
        .expect("rsi-xtask should run")
}

fn run_verify_with_base(repository: &Path, base: &str) -> Output {
    run_verify_with_base_and_arguments(repository, base, &[])
}

fn run_verify_with_base_and_arguments(repository: &Path, base: &str, arguments: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_rsi-xtask"))
        .current_dir(repository)
        .env("RSI_AGENT_NOTES_BASE", base)
        .arg("verify-agent-notes")
        .args(arguments)
        .output()
        .expect("rsi-xtask should run")
}

fn stderr(output: &Output) -> String {
    String::from_utf8(output.stderr.clone()).expect("diagnostics should be UTF-8")
}

fn git(repository: &Path, arguments: &[&str]) {
    let output = Command::new("git")
        .current_dir(repository)
        .args(arguments)
        .output()
        .expect("git should run");
    assert!(
        output.status.success(),
        "git {} failed: {}",
        arguments.join(" "),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn git_stdout(repository: &Path, arguments: &[&str]) -> String {
    let output = Command::new("git")
        .current_dir(repository)
        .args(arguments)
        .output()
        .expect("git should run");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout)
        .expect("git output should be UTF-8")
        .trim()
        .to_owned()
}

fn write_note(repository: &Path, relative: &str, contents: &str) {
    let path = repository.join(relative);
    fs::create_dir_all(path.parent().expect("note parent")).expect("note directory");
    fs::write(path, contents).expect("agent note");
}

#[test]
fn legacy_decision_home_is_rejected() {
    let repository = TempDir::new().expect("temporary repository");
    let legacy = repository.path().join("docs/adr");
    fs::create_dir_all(&legacy).expect("legacy directory");
    fs::write(legacy.join("0001-old.md"), "# Old decision\n").expect("legacy note");

    let output = run_verify(repository.path(), &[]);

    assert!(!output.status.success());
    assert!(stderr(&output).contains("legacy decision home `docs/adr`"));
}

#[test]
fn frontmatter_rejects_redundant_status_field() {
    let repository = TempDir::new().expect("temporary repository");
    write_note(
        repository.path(),
        ".agents/notes/proposed/architecture/2026-08-15-routing.md",
        "---\nname: Routing\nstatus: proposed\n---\n\n## Problem\n\nRouting is unclear.\n\n## Proposal\n\nUse snapshots.\n\n## Alternatives considered\n\nUse locks.\n\n## Acceptance criteria\n\nReaders use snapshots.\n\n## Risks\n\nSnapshots retain memory.\n",
    );

    let output = run_verify(repository.path(), &[]);

    assert!(!output.status.success());
    assert!(stderr(&output).contains("invalid frontmatter"));
}

#[test]
fn frontmatter_strings_must_be_nonempty_single_lines() {
    let cases = [
        ("name: '   '", "`name` must be a nonempty single line"),
        (
            "name: Routing\ncomment: ''",
            "`comment` must be a nonempty single line when present",
        ),
        (
            "name: |\n  Routing\n  decision",
            "`name` must be a nonempty single line",
        ),
    ];

    for (index, (frontmatter, expected)) in cases.into_iter().enumerate() {
        let repository = TempDir::new().expect("temporary repository");
        write_note(
            repository.path(),
            &format!(
                ".agents/notes/proposed/architecture/2026-08-1{}-routing.md",
                index + 5
            ),
            &format!(
                "---\n{frontmatter}\n---\n\n## Problem\n\nRouting is unclear.\n\n## Proposal\n\nUse snapshots.\n\n## Alternatives considered\n\nUse locks.\n\n## Acceptance criteria\n\nReaders use snapshots.\n\n## Risks\n\nSnapshots retain memory.\n"
            ),
        );

        let output = run_verify(repository.path(), &[]);

        assert!(!output.status.success(), "case {index} unexpectedly passed");
        assert!(
            stderr(&output).contains(expected),
            "case {index}: {}",
            stderr(&output)
        );
    }
}

#[test]
fn unknown_note_class_is_rejected() {
    let repository = TempDir::new().expect("temporary repository");
    write_note(
        repository.path(),
        ".agents/notes/proposed/refactor/2026-08-15-routing.md",
        "---\nname: Routing\n---\n\n## Problem\n\nRouting is unclear.\n\n## Proposal\n\nUse snapshots.\n\n## Alternatives considered\n\nUse locks.\n\n## Acceptance criteria\n\nReaders use snapshots.\n\n## Risks\n\nSnapshots retain memory.\n",
    );

    let output = run_verify(repository.path(), &[]);

    assert!(!output.status.success());
    assert!(stderr(&output).contains("unknown Agent Note class `refactor`"));
}

#[test]
fn note_filename_requires_a_real_date_and_kebab_topic() {
    let repository = TempDir::new().expect("temporary repository");
    write_note(
        repository.path(),
        ".agents/notes/proposed/architecture/2026-02-29-Routing.md",
        "---\nname: A title independent from the filename\n---\n\n## Problem\n\nRouting is unclear.\n\n## Proposal\n\nUse snapshots.\n\n## Alternatives considered\n\nUse locks.\n\n## Acceptance criteria\n\nReaders use snapshots.\n\n## Risks\n\nSnapshots retain memory.\n",
    );

    let output = run_verify(repository.path(), &[]);

    assert!(!output.status.success());
    assert!(stderr(&output).contains("invalid Agent Note filename `2026-02-29-Routing.md`"));
}

#[test]
fn proposed_note_requires_every_canonical_section() {
    let repository = TempDir::new().expect("temporary repository");
    write_note(
        repository.path(),
        ".agents/notes/proposed/feature/2026-08-15-snapshot-routing.md",
        "---\nname: Snapshot routing\ncomment: Explore immutable reads\n---\n\n## Problem\n\nRouting is unclear.\n\n## Proposal\n\nUse snapshots.\n\n## Alternatives considered\n\nUse locks.\n\n## Acceptance criteria\n\nReaders use snapshots.\n",
    );

    let output = run_verify(repository.path(), &[]);

    assert!(!output.status.success());
    assert!(stderr(&output).contains("missing required section `## Risks`"));
}

#[test]
fn canonical_sections_must_contain_prose() {
    let repository = TempDir::new().expect("temporary repository");
    write_note(
        repository.path(),
        ".agents/notes/implemented/architecture/2026-08-15-snapshot-routing.md",
        "---\nname: Snapshot routing\n---\n\n## Problem\n\nRouting is unclear.\n\n## Decision\n\n## Alternatives considered\n\nUse locks.\n\n## Consequences\n\nReads stay cheap.\n",
    );

    let output = run_verify(repository.path(), &[]);

    assert!(!output.status.success());
    assert!(stderr(&output).contains("section `## Decision` must contain content"));
}

#[test]
fn fenced_code_does_not_make_an_agent_note_section_nonempty() {
    let repository = TempDir::new().expect("temporary repository");
    write_note(
        repository.path(),
        ".agents/notes/implemented/testing/2026-08-16-empty-decision.md",
        "---\nname: Empty decision\n---\n\n## Problem\n\nProblem prose.\n\n## Decision\n\n```text\nplaceholder\n```\n\n## Alternatives considered\n\nAlternative prose.\n\n## Consequences\n\nConsequence prose.\n",
    );

    let output = run_verify(repository.path(), &[]);

    assert!(!output.status.success());
    assert!(stderr(&output).contains("section `## Decision` must contain content"));
}

#[test]
fn shorter_fence_does_not_close_a_longer_agent_note_fence() {
    let repository = TempDir::new().expect("temporary repository");
    write_note(
        repository.path(),
        ".agents/notes/implemented/testing/2026-08-16-long-fence.md",
        "---\nname: Long fence\n---\n\n## Problem\n\nProblem prose.\n\n## Decision\n\n````text\nplaceholder\n```\nstill fenced\n````\n\n## Alternatives considered\n\nAlternative prose.\n\n## Consequences\n\nConsequence prose.\n",
    );

    let output = run_verify(repository.path(), &[]);

    assert!(!output.status.success());
    assert!(stderr(&output).contains("section `## Decision` must contain content"));
}

#[test]
fn implemented_notes_reject_proposal_era_headings() {
    let repository = TempDir::new().expect("temporary repository");
    write_note(
        repository.path(),
        ".agents/notes/implemented/architecture/2026-08-15-snapshot-routing.md",
        "---\nname: Snapshot routing\n---\n\n## Problem\n\nRouting is unclear.\n\n## Decision\n\nUse snapshots.\n\n## Proposal\n\nMigrate later.\n\n## Alternatives considered\n\nUse locks.\n\n## Consequences\n\nReads stay cheap.\n",
    );

    let output = run_verify(repository.path(), &[]);

    assert!(!output.status.success());
    assert!(stderr(&output).contains("implemented note may not contain `## Proposal`"));
}

#[test]
fn frontmatter_is_the_title_and_problem_opens_the_body() {
    let repository = TempDir::new().expect("temporary repository");
    write_note(
        repository.path(),
        ".agents/notes/rejected/simplification/2026-08-15-snapshot-routing.md",
        "---\nname: Snapshot routing\ncomment: Locks are sufficient\n---\n\n# Agent Note: Snapshot routing\n\n## Problem\n\nRouting is unclear.\n\n## Proposal\n\nUse snapshots.\n\n## Alternatives considered\n\nKeep locks.\n",
    );

    let output = run_verify(repository.path(), &[]);

    assert!(!output.status.success());
    assert!(stderr(&output).contains("must not contain a level-one heading"));
}

#[test]
fn setext_level_one_heading_is_rejected() {
    let repository = TempDir::new().expect("temporary repository");
    write_note(
        repository.path(),
        ".agents/notes/rejected/simplification/2026-08-15-snapshot-routing.md",
        "---\nname: Snapshot routing\n---\n\nAgent Note: Snapshot routing\n============================\n\n## Problem\n\nRouting is unclear.\n\n## Proposal\n\nUse snapshots.\n\n## Alternatives considered\n\nKeep locks.\n",
    );

    let output = run_verify(repository.path(), &[]);

    assert!(!output.status.success());
    assert!(stderr(&output).contains("must not contain a level-one heading"));
}

#[test]
fn canonical_sections_may_not_repeat() {
    let repository = TempDir::new().expect("temporary repository");
    write_note(
        repository.path(),
        ".agents/notes/rejected/feature/2026-08-15-snapshot-routing.md",
        "---\nname: Snapshot routing\n---\n\n## Problem\n\nRouting is unclear.\n\n## Proposal\n\nUse snapshots.\n\n## Alternatives considered\n\nKeep locks.\n\n## Alternatives considered\n\nUse a queue.\n",
    );

    let output = run_verify(repository.path(), &[]);

    assert!(!output.status.success());
    assert!(
        stderr(&output).contains("section `## Alternatives considered` appears more than once")
    );
}

#[test]
fn unsealed_archived_note_is_rejected() {
    let repository = TempDir::new().expect("temporary repository");
    write_note(
        repository.path(),
        ".agents/notes/archived/architecture/2026-08-15-snapshot-routing.md",
        "---\nname: Snapshot routing\n---\n\n## Problem\n\nRouting was unclear.\n\n## Decision\n\nUse snapshots.\n\n## Alternatives considered\n\nUse locks.\n\n## Consequences\n\nReads stay cheap.\n",
    );
    fs::write(
        repository
            .path()
            .join(".agents/notes/archived/manifest.json"),
        "{\n  \"version\": 1,\n  \"notes\": {}\n}\n",
    )
    .expect("archive manifest");

    let output = run_verify(repository.path(), &[]);

    assert!(!output.status.success());
    assert!(
        stderr(&output)
            .contains("archived note `architecture/2026-08-15-snapshot-routing.md` is not sealed")
    );
}

#[test]
fn write_mode_appends_a_deterministic_archive_seal() {
    let repository = TempDir::new().expect("temporary repository");
    write_note(
        repository.path(),
        ".agents/notes/archived/architecture/2026-08-15-snapshot-routing.md",
        "---\nname: Snapshot routing\n---\n\n## Problem\n\nRouting was unclear.\n\n## Decision\n\nUse snapshots.\n\n## Alternatives considered\n\nUse locks.\n\n## Consequences\n\nReads stay cheap.\n",
    );
    let manifest = repository
        .path()
        .join(".agents/notes/archived/manifest.json");
    fs::write(&manifest, "{\n  \"version\": 1,\n  \"notes\": {}\n}\n").expect("archive manifest");

    let output = run_verify(repository.path(), &["--write"]);

    assert!(output.status.success(), "{}", stderr(&output));
    assert_eq!(
        fs::read_to_string(manifest).expect("written manifest"),
        concat!(
            "{\n",
            "  \"version\": 1,\n",
            "  \"notes\": {\n",
            "    \"architecture/2026-08-15-snapshot-routing.md\": ",
            "\"5453ce114b052be368d09b25a000071ec237abe158079de220a96446be469a23\"\n",
            "  }\n",
            "}\n",
        )
    );
}

#[test]
fn committed_archive_seals_are_append_only() {
    let repository = TempDir::new().expect("temporary repository");
    write_note(
        repository.path(),
        ".agents/notes/archived/architecture/2026-08-15-snapshot-routing.md",
        "---\nname: Snapshot routing\n---\n\n## Problem\n\nRouting was unclear.\n\n## Decision\n\nUse snapshots.\n\n## Alternatives considered\n\nUse locks.\n\n## Consequences\n\nReads stay cheap.\n",
    );
    let archive = repository.path().join(".agents/notes/archived");
    let manifest = archive.join("manifest.json");
    fs::write(&manifest, "{\n  \"version\": 1,\n  \"notes\": {}\n}\n").expect("archive manifest");
    assert!(run_verify(repository.path(), &["--write"]).status.success());
    git(repository.path(), &["init", "-q"]);
    git(repository.path(), &["config", "user.name", "Test"]);
    git(
        repository.path(),
        &["config", "user.email", "test@example.com"],
    );
    git(repository.path(), &["add", ".agents/notes/archived"]);
    git(
        repository.path(),
        &["commit", "--no-gpg-sign", "-qm", "seal archive"],
    );

    fs::remove_file(archive.join("architecture/2026-08-15-snapshot-routing.md"))
        .expect("remove archived note");
    fs::write(&manifest, "{\n  \"version\": 1,\n  \"notes\": {}\n}\n").expect("remove seal");

    let output = run_verify(repository.path(), &[]);

    assert!(!output.status.success());
    assert!(stderr(&output).contains(
        "archive manifest removed or replaced seal `architecture/2026-08-15-snapshot-routing.md`"
    ));
}

#[test]
fn unknown_lifecycle_directory_is_rejected() {
    let repository = TempDir::new().expect("temporary repository");
    write_note(
        repository.path(),
        ".agents/notes/draft/feature/2026-08-15-snapshot-routing.md",
        "---\nname: Snapshot routing\n---\n\n## Problem\n\nRouting is unclear.\n\n## Proposal\n\nUse snapshots.\n\n## Alternatives considered\n\nUse locks.\n\n## Acceptance criteria\n\nReaders use snapshots.\n\n## Risks\n\nSnapshots retain memory.\n",
    );

    let output = run_verify(repository.path(), &[]);

    assert!(!output.status.success());
    assert!(stderr(&output).contains("unexpected entry `.agents/notes/draft`"));
}

#[test]
fn lifecycle_requires_exact_class_and_note_depth() {
    let repository = TempDir::new().expect("temporary repository");
    write_note(
        repository.path(),
        ".agents/notes/proposed/2026-08-15-snapshot-routing.md",
        "---\nname: Snapshot routing\n---\n\n## Problem\n\nRouting is unclear.\n\n## Proposal\n\nUse snapshots.\n\n## Alternatives considered\n\nUse locks.\n\n## Acceptance criteria\n\nReaders use snapshots.\n\n## Risks\n\nSnapshots retain memory.\n",
    );

    let output = run_verify(repository.path(), &[]);

    assert!(!output.status.success());
    assert!(stderr(&output).contains("must contain only class directories"));
}

#[test]
fn only_a_full_zero_object_id_can_select_an_empty_ci_baseline() {
    let repository = TempDir::new().expect("temporary repository");

    let output = run_verify_with_base(repository.path(), "0");

    assert!(!output.status.success());
    assert!(stderr(&output).contains("Git reference `0` from RSI_AGENT_NOTES_BASE is unavailable"));
    let initial_push = run_verify_with_base(repository.path(), &"0".repeat(40));
    assert!(initial_push.status.success(), "{}", stderr(&initial_push));
}

#[test]
fn valid_notes_in_every_lifecycle_are_accepted() {
    let repository = TempDir::new().expect("temporary repository");
    write_note(
        repository.path(),
        ".agents/notes/proposed/feature/2026-08-15-snapshot-routing.md",
        "---\nname: A title independent of the topic slug\n---\n\n## Problem\n\nRouting is unclear.\n\n## Proposal\n\nUse snapshots.\n\n## Alternatives considered\n\nUse locks.\n\n## Acceptance criteria\n\nReaders use snapshots.\n\n## Risks\n\nSnapshots retain memory.\n",
    );
    write_note(
        repository.path(),
        ".agents/notes/implemented/testing/2026-08-16-deterministic-routing.md",
        "---\nname: Deterministic routing checks\ncomment: Explicit gates replace sleeps\n---\n\n## Problem\n\nOrdering needs evidence.\n\n## Decision\n\nUse explicit gates.\n\n## Alternatives considered\n\nUse wall-clock sleeps.\n\n## Consequences\n\nTests remain deterministic.\n\n```markdown\n# This is sample prose\n## Proposal\n## Acceptance criteria\n```\n",
    );
    write_note(
        repository.path(),
        ".agents/notes/rejected/bug-fix/2026-08-16-retry-loop.md",
        "---\nname: Retry every failure\ncomment: Typed failures already distinguish retries\n---\n\n## Problem\n\nRetries were inconsistent.\n\n## Proposal\n\nRetry every failure.\n\n## Alternatives considered\n\nRetry only classified transient failures.\n",
    );

    let output = run_verify(repository.path(), &[]);

    assert!(output.status.success(), "{}", stderr(&output));
}

#[test]
fn frontmatter_rejects_unknown_and_duplicate_fields() {
    let cases = [
        "name: Routing\nowner: platform",
        "name: Routing\nname: Duplicate",
    ];
    for (index, frontmatter) in cases.into_iter().enumerate() {
        let repository = TempDir::new().expect("temporary repository");
        write_note(
            repository.path(),
            &format!(
                ".agents/notes/proposed/process/2026-08-1{}-routing.md",
                index + 5
            ),
            &format!(
                "---\n{frontmatter}\n---\n\n## Problem\n\nRouting is unclear.\n\n## Proposal\n\nUse snapshots.\n\n## Alternatives considered\n\nUse locks.\n\n## Acceptance criteria\n\nReaders use snapshots.\n\n## Risks\n\nSnapshots retain memory.\n"
            ),
        );

        let output = run_verify(repository.path(), &[]);

        assert!(!output.status.success(), "case {index} unexpectedly passed");
        assert!(stderr(&output).contains("invalid frontmatter"));
    }
}

#[test]
fn canonical_sections_must_be_ordered() {
    let repository = TempDir::new().expect("temporary repository");
    write_note(
        repository.path(),
        ".agents/notes/proposed/process/2026-08-16-note-policy.md",
        "---\nname: Note policy\n---\n\n## Problem\n\nDecisions drift.\n\n## Alternatives considered\n\nUse ADRs.\n\n## Proposal\n\nUse Agent Notes.\n\n## Acceptance criteria\n\nThe gate passes.\n\n## Risks\n\nThe process adds work.\n",
    );

    let output = run_verify(repository.path(), &[]);

    assert!(!output.status.success());
    assert!(
        stderr(&output).contains("required section `## Alternatives considered` is out of order")
    );
}

#[test]
fn archive_hash_mismatch_is_rejected() {
    let repository = TempDir::new().expect("temporary repository");
    let relative = ".agents/notes/archived/architecture/2026-08-15-snapshot-routing.md";
    write_note(
        repository.path(),
        relative,
        "---\nname: Snapshot routing\n---\n\n## Problem\n\nRouting was unclear.\n\n## Decision\n\nUse snapshots.\n\n## Alternatives considered\n\nUse locks.\n\n## Consequences\n\nReads stay cheap.\n",
    );
    let manifest = repository
        .path()
        .join(".agents/notes/archived/manifest.json");
    fs::write(&manifest, "{\n  \"version\": 1,\n  \"notes\": {}\n}\n").expect("archive manifest");
    assert!(run_verify(repository.path(), &["--write"]).status.success());
    let note = repository.path().join(relative);
    let mut contents = fs::read_to_string(&note).expect("archived note");
    contents.push_str("\nAdditional history.\n");
    fs::write(note, contents).expect("tampered archived note");

    let output = run_verify(repository.path(), &[]);

    assert!(!output.status.success());
    assert!(stderr(&output).contains("does not match its seal"));
}

#[test]
fn archive_manifest_may_only_append_new_seals() {
    let repository = TempDir::new().expect("temporary repository");
    let archive = repository.path().join(".agents/notes/archived");
    write_note(
        repository.path(),
        ".agents/notes/archived/architecture/2026-08-15-snapshot-routing.md",
        "---\nname: Snapshot routing\n---\n\n## Problem\n\nRouting was unclear.\n\n## Decision\n\nUse snapshots.\n\n## Alternatives considered\n\nUse locks.\n\n## Consequences\n\nReads stay cheap.\n",
    );
    fs::write(
        archive.join("manifest.json"),
        "{\n  \"version\": 1,\n  \"notes\": {}\n}\n",
    )
    .expect("archive manifest");
    assert!(run_verify(repository.path(), &["--write"]).status.success());
    git(repository.path(), &["init", "-q"]);
    git(repository.path(), &["config", "user.name", "Test"]);
    git(
        repository.path(),
        &["config", "user.email", "test@example.com"],
    );
    git(repository.path(), &["add", ".agents/notes/archived"]);
    git(
        repository.path(),
        &["commit", "--no-gpg-sign", "-qm", "seal first archive"],
    );
    let base = git_stdout(repository.path(), &["rev-parse", "HEAD"]);
    write_note(
        repository.path(),
        ".agents/notes/archived/testing/2026-08-16-deterministic-routing.md",
        "---\nname: Deterministic routing\n---\n\n## Problem\n\nOrdering was unclear.\n\n## Decision\n\nUse explicit gates.\n\n## Alternatives considered\n\nUse sleeps.\n\n## Consequences\n\nTests are deterministic.\n",
    );

    let output = run_verify_with_base_and_arguments(repository.path(), &base, &["--write"]);

    assert!(output.status.success(), "{}", stderr(&output));
    let manifest = fs::read_to_string(archive.join("manifest.json")).expect("manifest");
    assert!(manifest.contains("architecture/2026-08-15-snapshot-routing.md"));
    assert!(manifest.contains("testing/2026-08-16-deterministic-routing.md"));
    assert!(run_verify(repository.path(), &[]).status.success());
}

#[test]
fn repository_without_head_uses_an_empty_archive_baseline() {
    let repository = TempDir::new().expect("temporary repository");
    git(repository.path(), &["init", "-q"]);
    fs::create_dir_all(repository.path().join(".agents/notes/archived"))
        .expect("archive directory");
    fs::write(
        repository
            .path()
            .join(".agents/notes/archived/manifest.json"),
        "{\n  \"version\": 1,\n  \"notes\": {}\n}\n",
    )
    .expect("archive manifest");

    let output = run_verify(repository.path(), &[]);

    assert!(output.status.success(), "{}", stderr(&output));
}
