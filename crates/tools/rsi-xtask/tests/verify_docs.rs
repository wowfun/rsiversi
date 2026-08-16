use std::fs;
use std::path::Path;
use std::process::{Command, Output};

use sha2::{Digest, Sha256};
use tempfile::TempDir;

fn write(repository: &Path, relative: &str, contents: &str) {
    let path = repository.join(relative);
    fs::create_dir_all(path.parent().expect("file parent")).expect("create parent directory");
    fs::write(path, contents).expect("write test file");
}

fn readme(name: &str, body: &str) -> String {
    format!("# {name}\n\n{body}\n")
}

fn manifest(name: &str) -> String {
    format!("[package]\nname = \"{name}\"\nversion = \"0.0.0\"\nedition = \"2024\"\n")
}

fn words(count: usize) -> String {
    (0..count).map(|_| "word").collect::<Vec<_>>().join(" ")
}

fn write_empty_archive(repository: &Path) {
    write(
        repository,
        ".agents/notes/archived/manifest.json",
        "{\n  \"version\": 1,\n  \"notes\": {}\n}\n",
    );
}

fn write_archived_note(repository: &Path, consequence: &str) -> &'static str {
    const NOTE_PATH: &str = ".agents/notes/archived/testing/2026-08-16-retired-testing-policy.md";
    let contents = format!(
        concat!(
            "---\n",
            "name: Retired testing policy\n",
            "---\n\n",
            "## Problem\n\nRetired problem.\n\n",
            "## Decision\n\nRetired decision.\n\n",
            "## Alternatives considered\n\nRetired alternative.\n\n",
            "## Consequences\n\n{}\n",
        ),
        consequence
    );
    write(repository, NOTE_PATH, &contents);
    let hash = format!("{:x}", Sha256::digest(contents.as_bytes()));
    write(
        repository,
        ".agents/notes/archived/manifest.json",
        &format!(
            concat!(
                "{{\n",
                "  \"version\": 1,\n",
                "  \"notes\": {{\n",
                "    \"testing/2026-08-16-retired-testing-policy.md\": \"{}\"\n",
                "  }}\n",
                "}}\n",
            ),
            hash
        ),
    );
    NOTE_PATH
}

fn valid_repository() -> TempDir {
    let repository = TempDir::new().expect("temporary repository");
    let root = repository.path();
    write(
        root,
        "Cargo.toml",
        "[workspace]\nmembers = []\nresolver = \"3\"\n",
    );

    for relative in [
        "AGENTS.md",
        "docs/AGENTS.md",
        "crates/AGENTS.md",
        "crates/tools/AGENTS.md",
        "crates/rsi-meta/AGENTS.md",
        "plugins/AGENTS.md",
        "plugins/rsi-meta/AGENTS.md",
        "fixtures/AGENTS.md",
        "fixtures/rsi-meta/AGENTS.md",
        "examples/AGENTS.md",
        "examples/rsi-meta/AGENTS.md",
        "schemas/AGENTS.md",
        "schemas/rsi-meta/AGENTS.md",
    ] {
        write(root, relative, "# Scope\n\nGovern this subtree.\n");
    }
    write(root, "README.md", "# Repository\n");
    write(root, "docs/architecture.md", "# Architecture\n");
    write(root, "crates/rsi-meta/README.md", "# Product\n");

    let packages = [
        (
            "crates/rsi-meta/core",
            "product",
            "A compact package contract with no level-two heading.",
        ),
        (
            "crates/tools/rsi-xtask",
            "tool",
            "This package exposes repository commands.\n\n## Commands\n\n```sh\ncargo xtask verify-docs\n```",
        ),
        (
            "plugins/rsi-meta/native",
            "native",
            "This plugin provides one native capability.\n\n## Retirement\n\nRetirement stops generation-owned work.",
        ),
        (
            "plugins/rsi-meta/support",
            "support",
            "Shared protocol types for maintained plugins and fixtures.",
        ),
        (
            "fixtures/rsi-meta/sample",
            "fixture",
            "This fixture provides black-box evidence.\n\n## Evidence\n\nIt observes only public behavior.",
        ),
    ];
    for (directory, name, body) in packages {
        write(root, &format!("{directory}/Cargo.toml"), &manifest(name));
        write(root, &format!("{directory}/README.md"), &readme(name, body));
    }
    write(
        root,
        "plugins/rsi-meta/native/plugin.toml",
        "name = \"native\"\n",
    );
    repository
}

fn run(repository: &Path, arguments: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_rsi-xtask"))
        .current_dir(repository)
        .env_remove("RSI_AGENT_NOTES_BASE")
        .args(arguments)
        .output()
        .expect("rsi-xtask should run")
}

fn verify(repository: &Path) -> Output {
    run(repository, &["verify-docs"])
}

fn stderr(output: &Output) -> String {
    String::from_utf8(output.stderr.clone()).expect("diagnostics should be UTF-8")
}

#[test]
fn content_driven_readmes_in_all_package_locations_and_commonmark_links_are_accepted() {
    let repository = valid_repository();
    write(
        repository.path(),
        "docs/architecture.md",
        concat!(
            "Architecture\n============\n\n",
            "## Über view\n\nFirst.\n\n",
            "## Über view\n\nSecond.\n\n",
            "```md\n## Not an anchor\n[broken](missing.md)\n```\n",
            "<a href=\"also-missing.md\">raw HTML is outside the gate</a>\n",
        ),
    );
    write(
        repository.path(),
        "README.md",
        concat!(
            "# Repository\n\n",
            "[encoded](docs/architecture.md#%C3%BCber-view)\n\n",
            "[root relative](/docs/architecture.md#über-view-1)\n\n",
            "[external](https://example.com/not-checked)\n",
        ),
    );

    let output = verify(repository.path());

    assert!(output.status.success(), "{}", stderr(&output));
}

#[test]
fn fragment_only_links_are_checked_against_the_source_document() {
    let repository = valid_repository();
    write(
        repository.path(),
        "README.md",
        "# Repository\n\n[missing same-document heading](#does-not-exist)\n",
    );

    let output = verify(repository.path());

    assert!(!output.status.success());
    assert!(stderr(&output).contains("Markdown heading fragment `#does-not-exist` does not exist"));
}

#[test]
fn agent_instruction_word_limits_are_inclusive_and_diagnostics_are_sorted() {
    let repository = valid_repository();
    write(repository.path(), "AGENTS.md", &words(400));
    write(repository.path(), "plugins/rsi-meta/AGENTS.md", &words(300));

    let at_limits = verify(repository.path());
    assert!(at_limits.status.success(), "{}", stderr(&at_limits));

    write(repository.path(), "AGENTS.md", &words(401));
    write(repository.path(), "plugins/rsi-meta/AGENTS.md", &words(301));

    let over_limits = verify(repository.path());
    let errors = stderr(&over_limits);
    assert!(!over_limits.status.success());
    assert!(errors.contains("AGENTS.md contains 401 words, exceeding its 400-word limit"));
    assert!(errors.contains("AGENTS.md contains 301 words, exceeding its 300-word limit"));
    let root = errors.find("AGENTS.md:1").expect("root budget error");
    let plugin = errors
        .find("plugins/rsi-meta/AGENTS.md:1")
        .expect("plugin budget error");
    assert!(root < plugin, "diagnostics should be path-sorted: {errors}");
}

#[test]
fn future_active_agent_instruction_files_are_discovered_without_registration() {
    let repository = valid_repository();
    write(repository.path(), "new-scope/AGENTS.md", &words(301));

    let output = verify(repository.path());
    let errors = stderr(&output);

    assert!(!output.status.success());
    assert!(errors.contains("new-scope/AGENTS.md:1"));
    assert!(errors.contains("exceeding its 300-word limit"));
}

#[test]
fn rsi_meta_docs_use_the_product_parent_governance() {
    let repository = valid_repository();

    let without_child = verify(repository.path());
    assert!(without_child.status.success(), "{}", stderr(&without_child));

    write(
        repository.path(),
        "crates/rsi-meta/docs/AGENTS.md",
        "# Redundant docs governance\n",
    );
    let with_child = verify(repository.path());

    assert!(!with_child.status.success());
    assert!(stderr(&with_child).contains(
        "rsi-meta docs governance is merged into crates/rsi-meta/AGENTS.md; the redundant boundary must not be reintroduced"
    ));
}

#[test]
fn readme_errors_and_unknown_packages_are_aggregated_and_sorted() {
    let repository = valid_repository();
    fs::remove_file(repository.path().join("crates/rsi-meta/core/README.md"))
        .expect("remove product README");
    write(
        repository.path(),
        "crates/tools/rsi-xtask/README.md",
        "# tool\n\nTool contract.\n\n# Tool details\n\nMore prose.\n",
    );
    write(
        repository.path(),
        "plugins/rsi-meta/native/README.md",
        "# wrong-name\n\nNative plugin contract.\n",
    );
    write(
        repository.path(),
        "plugins/rsi-meta/support/README.md",
        "## Shared protocol\n\nSupport package contract.\n",
    );
    write(
        repository.path(),
        "fixtures/rsi-meta/sample/README.md",
        concat!(
            "# fixture\n\n",
            "<!-- a comment is not package prose -->\n\n",
            "```sh\ncargo test\n```\n",
        ),
    );
    write(
        repository.path(),
        "misc/unknown/Cargo.toml",
        &manifest("unknown"),
    );

    let output = verify(repository.path());
    let errors = stderr(&output);

    assert!(!output.status.success());
    assert!(errors.contains("Cargo package must have a sibling README.md"));
    assert!(errors.contains("package README must contain exactly one level-one heading"));
    assert!(errors.contains("package README heading must match Cargo package name `native`"));
    assert!(errors.contains("package README must contain `# support`"));
    assert!(errors.contains("package README must contain a nonempty prose paragraph"));
    assert!(errors.contains("Cargo package path is not a recognized documentation class"));
    let core = errors
        .find("crates/rsi-meta/core/README.md")
        .expect("core error");
    let tool = errors
        .find("crates/tools/rsi-xtask/README.md")
        .expect("tool error");
    assert!(core < tool, "diagnostics should be path-sorted: {errors}");
}

#[test]
fn bad_case_repository_escape_and_symlink_escape_are_rejected() {
    let repository = valid_repository();
    let outside = TempDir::new().expect("outside directory");
    write(outside.path(), "outside.md", "# Outside\n");
    #[cfg(unix)]
    std::os::unix::fs::symlink(
        outside.path().join("outside.md"),
        repository.path().join("outside-link.md"),
    )
    .expect("outside symlink");
    write(
        repository.path(),
        "docs/architecture.md",
        concat!(
            "# Architecture\n\n",
            "[case](Architecture.md)\n\n",
            "[escape](../../outside.md)\n\n",
            "[symlink](/outside-link.md)\n",
        ),
    );

    let output = verify(repository.path());
    let errors = stderr(&output);

    assert!(!output.status.success());
    assert!(errors.contains("link path has incorrect case"));
    assert!(errors.contains("link target escapes the repository"));
    #[cfg(unix)]
    assert!(errors.contains("link target escapes the repository through a symlink"));
}

#[test]
fn ignored_trees_and_archived_markdown_are_not_scanned() {
    let repository = valid_repository();
    for relative in [
        ".local/broken.md",
        ".references/broken.md",
        ".git/broken.md",
        "target/broken.md",
        "crates/rsi-meta/core/target/broken.md",
    ] {
        write(repository.path(), relative, "[ignored](missing.md)\n");
    }
    for relative in [
        ".local/AGENTS.md",
        ".references/AGENTS.md",
        ".git/AGENTS.md",
        "target/AGENTS.md",
        "crates/rsi-meta/core/target/AGENTS.md",
    ] {
        write(repository.path(), relative, &words(401));
    }
    write_archived_note(repository.path(), "[ignored](missing.md)");

    let output = verify(repository.path());

    assert!(output.status.success(), "{}", stderr(&output));
}

#[test]
fn archive_does_not_require_local_governance() {
    let repository = valid_repository();
    write_empty_archive(repository.path());

    let output = verify(repository.path());

    assert!(output.status.success(), "{}", stderr(&output));
}

#[test]
fn archive_rejects_a_reintroduced_local_governance_file() {
    let repository = valid_repository();
    write_empty_archive(repository.path());
    write(
        repository.path(),
        ".agents/notes/archived/AGENTS.md",
        "# Redundant archive instructions\n",
    );

    let output = verify(repository.path());

    assert!(!output.status.success());
    assert!(stderr(&output).contains("unexpected archive entry `AGENTS.md`"));
}

#[test]
fn implemented_lifecycle_does_not_require_local_governance() {
    let repository = valid_repository();
    fs::create_dir_all(repository.path().join(".agents/notes/implemented"))
        .expect("create implemented lifecycle directory");

    let output = verify(repository.path());

    assert!(output.status.success(), "{}", stderr(&output));
}

#[test]
fn implemented_lifecycle_rejects_a_reintroduced_local_governance_file() {
    let repository = valid_repository();
    write(
        repository.path(),
        ".agents/notes/implemented/AGENTS.md",
        "# Redundant implemented instructions\n",
    );

    let output = verify(repository.path());

    assert!(!output.status.success());
    assert!(stderr(&output).contains("must contain only class directories"));
}

#[test]
fn active_documentation_cannot_link_to_the_archive() {
    let repository = valid_repository();
    let archived_note = write_archived_note(repository.path(), "Retired consequence.");
    write(
        repository.path(),
        "README.md",
        &format!("[stale rationale]({archived_note})\n"),
    );

    let output = verify(repository.path());

    assert!(!output.status.success());
    assert!(stderr(&output).contains("may not link to an archived Agent Note"));
}

#[test]
fn command_rejects_non_root_invocation_and_arguments() {
    let repository = valid_repository();

    let non_root = verify(&repository.path().join("docs"));
    assert!(!non_root.status.success());
    assert!(stderr(&non_root).contains("verify-docs must run from the repository root"));

    let arguments = run(repository.path(), &["verify-docs", "--write"]);
    assert!(!arguments.status.success());
    assert!(stderr(&arguments).contains("usage: rsi-xtask"));
}

#[test]
fn unexpected_docs_taxonomy_and_missing_governance_are_rejected() {
    let repository = valid_repository();
    fs::remove_file(repository.path().join("schemas/rsi-meta/AGENTS.md"))
        .expect("remove product governance file");
    write(
        repository.path(),
        "crates/rsi-meta/docs/rfcs/0001.md",
        "# Legacy decision\n",
    );

    let output = verify(repository.path());
    let errors = stderr(&output);

    assert!(!output.status.success());
    assert!(errors.contains("product namespace must define AGENTS.md"));
    assert!(errors.contains("unsupported docs taxonomy directory `rfcs`"));
    assert!(errors.contains("legacy decision or specification home is forbidden"));
}
