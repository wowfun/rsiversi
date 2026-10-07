use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;
use std::process::{Command, Output};

use serde::Deserialize;

fn repository() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .and_then(Path::parent)
        .expect("xtask is nested three levels below the repository")
}

fn run(cwd: &Path, arguments: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_rsi-xtask"))
        .args(arguments)
        .current_dir(cwd)
        .output()
        .expect("rsi-xtask should run")
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

const DIRECT_RSI_META_AUTHORITIES: [&str; 3] = [
    "cargo clippy --locked -p rsi-meta",
    "cargo test --locked -p rsi-meta",
    "fixtures/rsi-meta/foundation-probe/Cargo.toml",
];

fn direct_rsi_meta_authorities(workflow: &str) -> Vec<&'static str> {
    DIRECT_RSI_META_AUTHORITIES
        .into_iter()
        .filter(|command| workflow.contains(command))
        .collect()
}

#[test]
fn rsi_meta_commands_are_recognized_and_require_the_repository_root() {
    let directory = tempfile::tempdir().unwrap();

    let output = run(directory.path(), &["rsi-meta", "conformance"]);
    assert!(!output.status.success());
    let error = stderr(&output);
    assert!(
        error.contains("must run from the repository root"),
        "unexpected conformance error: {error}"
    );
    assert!(!error.contains("usage: rsi-xtask"));
}

#[test]
fn rsi_meta_commands_reject_extra_arguments() {
    let output = run(repository(), &["rsi-meta", "conformance", "--unexpected"]);
    assert!(!output.status.success());
    assert!(stderr(&output).contains("usage: rsi-xtask"));
}

#[test]
fn ci_delegates_rsi_meta_enumeration_only_to_conformance() {
    let workflow = fs::read_to_string(repository().join(".github/workflows/ci.yml")).unwrap();
    assert_eq!(
        workflow.matches("cargo xtask rsi-meta conformance").count(),
        2,
        "the Unix matrix and Windows job must invoke the same authority"
    );
    assert_eq!(
        direct_rsi_meta_authorities(&workflow),
        Vec::<&str>::new(),
        "CI retained a second rsi-meta authority"
    );
    assert!(
        !workflow.contains("--exclude rsi-meta"),
        "product jobs must select their packages instead of reconstructing a workspace complement"
    );
}

#[test]
fn ci_pins_rsi_meta_conformance_to_linux_x86_64_and_macos_arm64() {
    #[derive(Debug, Deserialize, Eq, PartialEq)]
    struct Runner {
        os: String,
        expected_arch: String,
    }

    #[derive(Deserialize)]
    struct Matrix {
        include: Vec<Runner>,
    }

    #[derive(Deserialize)]
    struct Strategy {
        matrix: Matrix,
    }

    #[derive(Deserialize)]
    struct Step {
        name: Option<String>,
        run: Option<String>,
    }

    #[derive(Deserialize)]
    struct Job {
        strategy: Strategy,
        steps: Vec<Step>,
    }

    #[derive(Deserialize)]
    struct Workflow {
        jobs: BTreeMap<String, serde_json::Value>,
    }

    let workflow = fs::read_to_string(repository().join(".github/workflows/ci.yml")).unwrap();
    let workflow: Workflow = yaml_serde::from_str(&workflow).expect("workflow YAML");
    let conformance: Job = serde_json::from_value(workflow.jobs["rsi-meta-conformance"].clone())
        .expect("rsi-meta-conformance job");
    assert_eq!(
        conformance.strategy.matrix.include,
        vec![
            Runner {
                os: "ubuntu-24.04".into(),
                expected_arch: "x86_64".into(),
            },
            Runner {
                os: "macos-15".into(),
                expected_arch: "arm64".into(),
            },
        ],
        "rsi-meta conformance must retain one pinned native runner per supported Unix architecture"
    );
    let architecture_check = conformance
        .steps
        .iter()
        .find(|step| step.name.as_deref() == Some("Verify runner architecture"))
        .and_then(|step| step.run.as_deref());
    assert_eq!(
        architecture_check,
        Some(r#"test "$(uname -m)" = "${{ matrix.expected_arch }}""#),
        "the conformance matrix must fail closed when a hosted-runner label changes architecture"
    );
}

#[test]
fn ci_exercises_every_agent_package_and_formats_the_repository_once() {
    let workflow = fs::read_to_string(repository().join(".github/workflows/ci.yml")).unwrap();
    assert!(
        workflow.contains("cargo clippy --locked -p 'rsi-agent*' --all-targets -- -D warnings")
    );
    assert!(workflow.contains("cargo test --locked -p 'rsi-agent*' --all-targets"));
    assert!(!workflow.contains(
        "cargo test --locked -p rsi-agent-session-protocol --all-targets --features serde_json/arbitrary_precision"
    ));
    let retired_protocol = ["rsi-agent", "protocol"].join("-");
    assert!(!workflow.contains(&retired_protocol));
    assert_eq!(workflow.matches("cargo fmt --all --check").count(), 1);
    let repository_tools = workflow
        .split_once("  repository-tools:\n")
        .and_then(|(_, suffix)| suffix.split_once("\n  dependency-audit:"))
        .map(|(job, _)| job)
        .expect("repository-tools job boundaries");
    assert!(
        repository_tools.contains("components: clippy, rustfmt"),
        "the job invoking cargo fmt must explicitly install rustfmt"
    );
}

#[test]
fn every_workspace_package_belongs_to_one_ci_failure_domain() {
    #[derive(Deserialize)]
    struct Workflow {
        jobs: BTreeMap<String, Job>,
    }

    #[derive(Deserialize)]
    struct Job {
        #[serde(default)]
        steps: Vec<Step>,
    }

    #[derive(Deserialize)]
    struct Step {
        run: Option<String>,
    }

    let metadata = workspace_metadata();
    let packages = metadata["packages"]
        .as_array()
        .expect("metadata packages array");
    let meta_packages = BTreeSet::from([
        "rsi-meta",
        "rsi-meta-contract",
        "rsi-meta-execution",
        "rsi-meta-native-loader",
        "rsi-meta-native",
        "rsi-meta-profile",
        "rsi-meta-scope",
    ]);
    let workflow = fs::read_to_string(repository().join(".github/workflows/ci.yml")).unwrap();
    let workflow: Workflow = yaml_serde::from_str(&workflow).expect("workflow YAML");
    let package_jobs = [
        "rsi-base",
        "rsi-ssh-linux",
        "rsi-ai",
        "rsi-agent",
        "rsi",
        "rsi-desktop",
        "repository-tools",
    ];
    for check in ["test", "clippy"] {
        let job_patterns = package_jobs
            .into_iter()
            .map(|job_name| {
                let patterns = workflow.jobs[job_name]
                    .steps
                    .iter()
                    .filter_map(|step| step.run.as_deref())
                    .flat_map(|command| cargo_package_patterns(command, check))
                    .collect::<BTreeSet<_>>();
                (job_name, patterns)
            })
            .collect::<BTreeMap<_, _>>();

        for package in packages {
            let name = package["name"].as_str().expect("package name");
            let manifest = Path::new(package["manifest_path"].as_str().expect("manifest path"));
            let relative = manifest
                .strip_prefix(repository())
                .expect("workspace manifest below repository");
            let mut components = relative.components();
            let owner = components
                .next()
                .and_then(|value| value.as_os_str().to_str());
            assert!(
                matches!(owner, Some("crates" | "apps")),
                "workspace package {name} escaped crates/ and apps/"
            );
            let product = components
                .next()
                .and_then(|value| value.as_os_str().to_str())
                .expect("product directory");
            if owner == Some("crates") && product == "rsi-meta" {
                assert!(
                    meta_packages.contains(name),
                    "rsi-meta package {name} is absent from the conformance authority"
                );
                continue;
            }
            let owners = job_patterns
                .iter()
                .filter(|(_, patterns)| {
                    patterns
                        .iter()
                        .any(|pattern| package_matches(pattern, name))
                })
                .map(|(job, _)| *job)
                .collect::<Vec<_>>();
            assert_eq!(
                owners.len(),
                1,
                "workspace package {name} at {} has {check} coverage in CI jobs {owners:?}",
                relative.display()
            );
        }
    }
}

fn workspace_metadata() -> serde_json::Value {
    let output = Command::new("cargo")
        .args(["metadata", "--no-deps", "--format-version", "1"])
        .current_dir(repository())
        .output()
        .expect("cargo metadata should run");
    assert!(
        output.status.success(),
        "cargo metadata failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).expect("cargo metadata JSON")
}

fn cargo_package_patterns(command: &str, check: &str) -> Vec<String> {
    command
        .lines()
        .flat_map(|line| {
            let tokens = line.split_whitespace().collect::<Vec<_>>();
            if !tokens.windows(2).any(|pair| pair == ["cargo", check])
                || !tokens.contains(&"--all-targets")
                || tokens
                    .iter()
                    .any(|token| matches!(*token, "--no-run" | "--ignored" | "--test" | "--doc"))
            {
                return Vec::new();
            }
            tokens
                .windows(2)
                .filter(|pair| pair[0] == "-p" || pair[0] == "--package")
                .map(|pair| pair[1].trim_matches(['\'', '"']).to_owned())
                .collect()
        })
        .collect()
}
#[test]
fn targeted_preflight_does_not_reassign_whole_package_ci_ownership() {
    let preflight = "cargo test --locked -p rsi-sandbox-local --test local native_enforcement -- --ignored --exact";
    assert!(cargo_package_patterns(preflight, "test").is_empty());
    assert_eq!(
        cargo_package_patterns(
            &format!(
                "{preflight}\ncargo clippy --locked -p rsi-desktop --all-targets -- -D warnings"
            ),
            "clippy"
        ),
        vec!["rsi-desktop"]
    );
}

#[test]
fn whole_package_coverage_requires_execution_and_the_matching_check() {
    for command in [
        "cargo test -p rsi-execution --all-targets --no-run",
        "cargo test -p rsi-execution --all-targets -- --ignored",
        "cargo clippy -p rsi-execution --all-targets -- -D warnings",
    ] {
        assert!(cargo_package_patterns(command, "test").is_empty());
    }
    assert_eq!(
        cargo_package_patterns("cargo test -p 'rsi-execution*' --all-targets", "test"),
        ["rsi-execution*"]
    );
}

fn package_matches(pattern: &str, package: &str) -> bool {
    pattern
        .strip_suffix('*')
        .map_or_else(|| package == pattern, |prefix| package.starts_with(prefix))
}

#[test]
fn ci_events_separate_pull_requests_main_pushes_and_manual_runs() {
    let source = fs::read_to_string(repository().join(".github/workflows/ci.yml")).unwrap();
    let workflow: yaml_serde::Value = yaml_serde::from_str(&source).unwrap();
    let events = workflow["on"].as_mapping().unwrap();
    assert_eq!(
        events
            .keys()
            .map(|key| key.as_str().unwrap())
            .collect::<BTreeSet<_>>(),
        BTreeSet::from(["pull_request", "push", "workflow_dispatch"])
    );
    assert_eq!(workflow["on"]["push"]["branches"][0].as_str(), Some("main"));
    assert_eq!(
        workflow["on"]["push"]["branches"]
            .as_sequence()
            .unwrap()
            .len(),
        1
    );
    assert!(workflow["on"]["pull_request"].is_null());
    assert!(workflow["on"]["workflow_dispatch"].is_null());
    assert_eq!(
        workflow["concurrency"]["group"].as_str(),
        Some(
            "${{ github.workflow }}-${{ github.event_name }}-${{ github.event.pull_request.number || github.ref }}"
        )
    );
    assert_eq!(
        workflow["concurrency"]["cancel-in-progress"].as_bool(),
        Some(true)
    );
    let documentation = workflow["jobs"]["documentation"]["steps"]
        .as_sequence()
        .unwrap();
    for command in [
        "cargo xtask verify-docs --structure-only",
        "cargo xtask verify-architecture",
        "cargo xtask verify-agent-notes",
    ] {
        let verify = documentation
            .iter()
            .find(|step| step["run"].as_str() == Some(command))
            .unwrap();
        assert!(verify["if"].as_str().unwrap().contains("!cancelled()"));
    }
    let notes = documentation
        .iter()
        .find(|step| step["name"].as_str() == Some("Verify Agent Notes"))
        .unwrap();
    assert_eq!(
        notes["env"]["RSI_AGENT_NOTES_BASE"].as_str(),
        Some("${{ steps.notes_base.outputs.base }}")
    );
}

#[test]
#[cfg(unix)]
fn ci_audit_selects_only_tracked_lockfiles_and_fetches_advisories_once() {
    use std::os::unix::fs::PermissionsExt;

    let source = fs::read_to_string(repository().join(".github/workflows/ci.yml")).unwrap();
    let workflow: yaml_serde::Value = yaml_serde::from_str(&source).unwrap();
    let steps = workflow["jobs"]["dependency-audit"]["steps"]
        .as_sequence()
        .unwrap();
    let audit = steps
        .iter()
        .find(|step| step["name"].as_str() == Some("Audit every committed Cargo lockfile"))
        .unwrap();
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    for path in [
        "Cargo.lock",
        "nested with spaces/Cargo.lock",
        "untracked/Cargo.lock",
        "target/Cargo.lock",
    ] {
        let path = root.join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, "fixture").unwrap();
    }
    assert!(
        Command::new("git")
            .args(["init", "--quiet"])
            .current_dir(root)
            .status()
            .unwrap()
            .success()
    );
    assert!(
        Command::new("git")
            .args(["add", "Cargo.lock", "nested with spaces/Cargo.lock"])
            .current_dir(root)
            .status()
            .unwrap()
            .success()
    );
    let bin = root.join("bin");
    fs::create_dir(&bin).unwrap();
    let cargo = bin.join("cargo");
    fs::write(&cargo, "#!/usr/bin/env python3\nimport json, sys\nprint(json.dumps([arg.removeprefix('./') for arg in sys.argv[1:]]))\n").unwrap();
    fs::set_permissions(&cargo, fs::Permissions::from_mode(0o700)).unwrap();
    let path = std::env::join_paths(
        std::iter::once(bin).chain(std::env::split_paths(&std::env::var_os("PATH").unwrap())),
    )
    .unwrap();
    let output = Command::new("bash")
        .args([
            "--noprofile",
            "--norc",
            "-e",
            "-o",
            "pipefail",
            "-c",
            audit["run"].as_str().unwrap(),
        ])
        .current_dir(root)
        .env("PATH", path)
        .output()
        .unwrap();
    assert!(output.status.success(), "{}", stderr(&output));
    let calls = String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<Vec<String>>(line).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(
        calls,
        vec![
            vec!["audit", "--file", "Cargo.lock"],
            vec![
                "audit",
                "--no-fetch",
                "--file",
                "nested with spaces/Cargo.lock"
            ],
        ]
    );
}

#[test]
fn ci_required_aggregates_every_independent_job_result() {
    #[derive(Deserialize)]
    struct Workflow {
        jobs: BTreeMap<String, Job>,
    }

    #[derive(Deserialize)]
    struct Job {
        #[serde(rename = "if")]
        condition: Option<String>,
        #[serde(default)]
        needs: Vec<String>,
        #[serde(default)]
        steps: Vec<Step>,
    }

    #[derive(Deserialize)]
    struct Step {
        name: Option<String>,
        #[serde(default)]
        env: BTreeMap<String, String>,
        shell: Option<String>,
        run: Option<String>,
    }

    let workflow = fs::read_to_string(repository().join(".github/workflows/ci.yml")).unwrap();
    let workflow: Workflow = yaml_serde::from_str(&workflow).expect("workflow YAML");
    let all_jobs = workflow
        .jobs
        .keys()
        .map(String::as_str)
        .collect::<BTreeSet<_>>();
    let expected = all_jobs
        .iter()
        .copied()
        .filter(|job| *job != "ci-required")
        .collect::<BTreeSet<_>>();
    let aggregate = workflow.jobs.get("ci-required").expect("ci-required job");
    assert_eq!(aggregate.condition.as_deref(), Some("always()"));
    let needs = aggregate
        .needs
        .iter()
        .map(String::as_str)
        .collect::<BTreeSet<_>>();
    assert_eq!(
        aggregate.needs.len(),
        needs.len(),
        "ci-required repeats one of its needed jobs"
    );
    assert_eq!(needs, expected, "ci-required omitted or invented a job");

    let contract = aggregate
        .steps
        .iter()
        .find(|step| step.name.as_deref() == Some("Require every CI contract"))
        .expect("aggregate contract step");
    assert_eq!(
        contract.env,
        BTreeMap::from([("NEEDS_JSON".into(), "${{ toJSON(needs) }}".into())]),
        "the aggregate consumes every result without a second job inventory"
    );
    assert_eq!(contract.shell.as_deref(), Some("python3 {0}"));
    let run = contract.run.as_deref().expect("aggregate contract script");
    let execute = |input: &serde_json::Value| {
        Command::new("python3")
            .args(["-c", run])
            .env("NEEDS_JSON", input.to_string())
            .output()
            .expect("run the workflow's aggregate script")
    };
    let successful: serde_json::Value = expected
        .iter()
        .map(|job| ((*job).to_owned(), serde_json::json!({"result": "success"})))
        .collect::<serde_json::Map<_, _>>()
        .into();
    let output = execute(&successful);
    assert!(output.status.success(), "{}", stderr(&output));
    for job in &expected {
        assert!(String::from_utf8_lossy(&output.stdout).contains(job));
        for result in [
            serde_json::json!({"result": "failure"}),
            serde_json::json!({"result": "cancelled"}),
            serde_json::json!({"result": "skipped"}),
            serde_json::json!({"result": "unknown"}),
            serde_json::json!({"result": null}),
            serde_json::json!({}),
        ] {
            let mut input = successful.clone();
            input[job] = result;
            assert!(!execute(&input).status.success(), "accepted {input}");
        }
    }
    assert!(!execute(&serde_json::json!({})).status.success());
}

#[test]
fn ci_frontend_smoke_retains_the_required_sandbox_policy_until_exit() {
    let source = fs::read_to_string(repository().join(".github/workflows/ci.yml")).unwrap();
    let workflow: yaml_serde::Value = yaml_serde::from_str(&source).unwrap();
    let steps = workflow["jobs"]["rsi"]["steps"].as_sequence().unwrap();
    let step = steps
        .iter()
        .find(|step| {
            step["run"].as_str().is_some_and(|run| {
                run.contains("cargo run --locked -p rsi-app-tools -- dev tui --smoke")
            })
        })
        .expect("product CI exercises the actual development launcher");
    let condition = step["if"].as_str().unwrap();
    for prerequisite in [
        "!cancelled()",
        "runner.os == 'Linux'",
        "steps.standard_build.outcome == 'success'",
    ] {
        assert!(
            condition.contains(prerequisite),
            "smoke omitted prerequisite: {prerequisite}"
        );
    }
    let run = step["run"].as_str().unwrap();
    let smoke = run
        .find("cargo run --locked -p rsi-app-tools -- dev tui --smoke")
        .unwrap();
    for setup in [
        "trap restore_policy EXIT",
        "kernel.unprivileged_userns_clone=1",
        "kernel.apparmor_restrict_unprivileged_userns=0",
    ] {
        assert!(
            run.find(setup).is_some_and(|index| index < smoke),
            "development smoke needs scoped backend policy: {setup}"
        );
    }
}

#[test]
fn ci_job_deadlines_cover_step_budgets_within_the_hosted_runner_limit() {
    let workflow = fs::read_to_string(repository().join(".github/workflows/ci.yml")).unwrap();
    assert_ci_job_deadlines(&workflow);
}

#[test]
fn ci_job_deadlines_accept_the_hosted_runner_ceiling() {
    assert_ci_job_deadlines(
        "jobs:
  bounded:
    timeout-minutes: 360
    steps:
      - run: cargo test
        timeout-minutes: 350
  unbounded:
    timeout-minutes: 360
    steps:
      - run: cargo test
",
    );
}

#[test]
#[should_panic(expected = "fixture job budget 361m exceeds the 360m GitHub-hosted runner limit")]
fn ci_job_deadlines_reject_over_limit_despite_sufficient_headroom() {
    assert_ci_job_deadlines(
        "jobs:
  fixture:
    timeout-minutes: 361
    steps:
      - run: cargo test
        timeout-minutes: 350
",
    );
}

#[test]
#[should_panic(expected = "fixture job budget 361m exceeds the 360m GitHub-hosted runner limit")]
fn ci_job_deadlines_reject_over_limit_without_explicit_step_budgets() {
    assert_ci_job_deadlines(
        "jobs:
  fixture:
    timeout-minutes: 361
    steps:
      - run: cargo test
",
    );
}

fn assert_ci_job_deadlines(workflow: &str) {
    #[derive(Deserialize)]
    struct Workflow {
        jobs: BTreeMap<String, Job>,
    }

    #[derive(Deserialize)]
    struct Job {
        #[serde(rename = "timeout-minutes")]
        timeout_minutes: Option<u64>,
        #[serde(default)]
        steps: Vec<Step>,
    }

    #[derive(Deserialize)]
    struct Step {
        run: Option<String>,
        #[serde(rename = "timeout-minutes")]
        timeout_minutes: Option<u64>,
    }

    let workflow: Workflow = yaml_serde::from_str(workflow).expect("workflow YAML");
    for (name, job) in workflow.jobs {
        if let Some(job_budget) = job.timeout_minutes {
            assert!(
                job_budget <= 360,
                "{name} job budget {job_budget}m exceeds the 360m GitHub-hosted runner limit"
            );
        }
        let explicit_step_budget = job
            .steps
            .iter()
            .filter_map(|step| step.timeout_minutes)
            .sum::<u64>();
        if name == "rsi-meta-browser" {
            assert!(
                job.steps
                    .iter()
                    .filter(|step| step.run.is_some())
                    .all(|step| step.timeout_minutes.is_some()),
                "browser commands require explicit deadlines"
            );
        }
        if explicit_step_budget == 0 {
            continue;
        }
        let job_budget = job
            .timeout_minutes
            .unwrap_or_else(|| panic!("{name} has bounded steps but no job deadline"));
        assert!(
            job_budget >= explicit_step_budget + 10,
            "{name} job budget {job_budget}m does not cover {explicit_step_budget}m of explicit steps plus 10m setup headroom"
        );
    }
}

#[test]
fn direct_foundation_probe_invocation_is_a_second_ci_authority() {
    let workflow =
        "cargo run --locked --manifest-path fixtures/rsi-meta/foundation-probe/Cargo.toml";
    assert_eq!(
        direct_rsi_meta_authorities(workflow),
        vec!["fixtures/rsi-meta/foundation-probe/Cargo.toml"]
    );
}

#[test]
fn gui_jobs_exercise_document_types_and_native_failure_boundaries() {
    #[derive(Deserialize)]
    struct Workflow {
        jobs: BTreeMap<String, Job>,
    }
    #[derive(Deserialize)]
    struct Job {
        steps: Vec<Step>,
    }
    #[derive(Deserialize)]
    struct Step {
        run: Option<String>,
    }
    let source = fs::read_to_string(repository().join(".github/workflows/ci.yml")).unwrap();
    let workflow: Workflow = yaml_serde::from_str(&source).unwrap();
    let scripts = |name: &str| {
        workflow.jobs[name]
            .steps
            .iter()
            .filter_map(|step| step.run.as_deref())
            .collect::<Vec<_>>()
            .join("\n")
    };
    let browser = scripts("rsi-meta-browser");
    for command in [
        "pnpm -C ../../../apps/web typecheck",
        "pnpm -C ../../../apps/web test",
    ] {
        assert!(
            browser.contains(command),
            "document check omitted: {command}"
        );
    }
    let desktop = scripts("rsi-desktop");
    for seam in [
        "fixtures/rsi/desktop-admission/Cargo.toml",
        "--foreign-bundle",
        "--ack-timeout",
        "--close-timeout",
        "--save-failure",
        "--restart",
        "--refresh-during-click",
    ] {
        assert!(desktop.contains(seam), "native boundary omitted: {seam}");
    }
}

#[test]
fn evaluation_evidence_is_independent_of_standard_tests_and_always_retained() {
    let source = fs::read_to_string(repository().join(".github/workflows/ci.yml")).unwrap();
    let workflow: yaml_serde::Value = yaml_serde::from_str(&source).unwrap();
    let jobs = workflow["jobs"].as_mapping().unwrap();
    let steps = jobs
        .values()
        .find_map(|job| {
            let steps = job["steps"].as_sequence()?;
            steps
                .iter()
                .any(|step| step["id"].as_str() == Some("session_eval_build"))
                .then_some(steps)
        })
        .unwrap();
    let evaluation = steps
        .iter()
        .find(|step| {
            step["run"]
                .as_str()
                .is_some_and(|run| run.contains("session_api.py --self-test"))
        })
        .unwrap();
    let run = evaluation["run"].as_str().unwrap();
    assert!(!run.contains("cargo test") && !run.contains("dev tui"));
    assert!(
        !run.contains("python-tests.py"),
        "eval unit failures must not suppress oracle/API evidence"
    );
    let unit = steps
        .iter()
        .find(|step| {
            step["run"]
                .as_str()
                .is_some_and(|run| run.contains("python-tests.py crates/rsi/core/eval"))
        })
        .unwrap();
    assert!(unit["if"].as_str().unwrap().contains("!cancelled()"));
    assert!(evaluation["if"].as_str().unwrap().contains("!cancelled()"));
    assert!(evaluation["timeout-minutes"].as_u64().unwrap() >= 3 * 8 + 20);
    let upload = steps
        .iter()
        .find(|step| step["with"]["name"].as_str() == Some("rsi-session-api-evaluation"))
        .unwrap();
    assert!(upload["if"].as_str().unwrap().contains("always()"));
    assert!(
        upload["with"]["path"]
            .as_str()
            .unwrap()
            .contains("rsi-session-api-evaluation")
    );
}

#[test]
fn paired_web_consumers_have_independent_failure_domains() {
    let source = fs::read_to_string(repository().join(".github/workflows/ci.yml")).unwrap();
    let workflow: yaml_serde::Value = yaml_serde::from_str(&source).unwrap();
    let steps = workflow["jobs"]["rsi-meta-browser"]["steps"]
        .as_sequence()
        .unwrap();
    let step = |id| {
        steps
            .iter()
            .find(|step| step["id"].as_str() == Some(id))
            .unwrap()
    };
    let harness = step("web_harness");
    assert_eq!(
        harness["working-directory"].as_str(),
        Some("fixtures/rsi/web-product")
    );
    assert!(
        harness["run"]
            .as_str()
            .unwrap()
            .contains("npm ci --ignore-scripts")
    );
    let build = step("web_build");
    assert!(
        build["run"]
            .as_str()
            .unwrap()
            .contains("pnpm -C ../../../apps/web build")
    );
    assert!(!build["run"].as_str().unwrap().contains("sudo sysctl"));
    for id in [
        "web_product",
        "web_workflow",
        "web_integrations",
        "web_terminals",
    ] {
        let consumer = step(id);
        let condition = consumer["if"].as_str().unwrap();
        assert!(condition.contains("!cancelled()"));
        assert!(condition.contains("steps.web_build.outcome == 'success'"));
        assert!(condition.contains("steps.web_harness.outcome == 'success'"));
        assert!(!condition.contains("steps.web_product.outcome"));
        assert!(!condition.contains("steps.web_integrations.outcome"));
        assert!(
            consumer["run"]
                .as_str()
                .unwrap()
                .contains("trap restore_policy EXIT")
        );
    }
    let product = step("web_product")["run"].as_str().unwrap();
    for probe in ["navigation-order.mjs", "device-storage.mjs"] {
        assert!(
            product.contains(&format!("node {probe}")),
            "missing deterministic product probe {probe}"
        );
    }
    let probes = step("web_integrations")["run"].as_str().unwrap();
    assert!(probes.contains("run-paired.py"));
    assert!(probes.contains("--ignored --exact --list"));
    assert!(
        !step("web_product")["run"]
            .as_str()
            .unwrap()
            .contains("run-paired.py")
    );
}

#[test]
fn paired_web_failures_publish_evidence_before_integration_probes() {
    let source = fs::read_to_string(repository().join(".github/workflows/ci.yml")).unwrap();
    let workflow: yaml_serde::Value = yaml_serde::from_str(&source).unwrap();
    let steps = workflow["jobs"]["rsi-meta-browser"]["steps"]
        .as_sequence()
        .unwrap();
    let step = |id| {
        steps
            .iter()
            .find(|step| step["id"].as_str() == Some(id))
            .unwrap()
    };
    let probes_index = steps
        .iter()
        .position(|step| step["id"] == "web_integrations")
        .unwrap();
    for (id, artifact, log) in [
        (
            "web_product",
            "rsi-web-product-failure-log",
            "web_product.log",
        ),
        (
            "web_workflow",
            "rsi-workflow-failure-evidence",
            "web_workflow.log",
        ),
    ] {
        let consumer_index = steps.iter().position(|step| step["id"] == id).unwrap();
        let log_index = steps
            .iter()
            .position(|step| step["with"]["name"] == artifact)
            .unwrap();
        assert!(consumer_index < log_index && log_index < probes_index);
        assert_eq!(
            steps[log_index]["if"].as_str().unwrap(),
            format!("${{{{ !cancelled() && steps.{id}.outcome == 'failure' }}}}")
        );
        assert!(
            step(id)["run"]
                .as_str()
                .unwrap()
                .contains(&format!("tee \"$RUNNER_TEMP/rsi-browser-logs/{log}\""))
        );
        assert!(
            steps[log_index]["with"]["path"]
                .as_str()
                .unwrap()
                .lines()
                .any(|line| line == format!("${{{{ runner.temp }}}}/rsi-browser-logs/{log}"))
        );
    }
    let archive_path = |name| {
        steps
            .iter()
            .find(|step| step["with"]["name"] == name)
            .unwrap()["with"]["path"]
            .as_str()
            .unwrap()
    };
    let workflow_archive = archive_path("rsi-workflow-failure-evidence");
    assert!(
        workflow_archive
            .lines()
            .any(|line| line == "${{ runner.temp }}/rsi-web-evidence/workflow-dock")
    );
    for archive in [workflow_archive, archive_path("rsi-web-evidence")] {
        assert!(
            archive
                .lines()
                .any(|line| line == "!${{ runner.temp }}/rsi-web-evidence/workflow-dock/rsi")
        );
    }
}

#[test]
fn browser_acceptance_has_explicit_independent_prerequisites_and_outcomes() {
    let source = fs::read_to_string(repository().join(".github/workflows/ci.yml")).unwrap();
    let workflow: yaml_serde::Value = yaml_serde::from_str(&source).unwrap();
    let steps = workflow["jobs"]["rsi-meta-browser"]["steps"]
        .as_sequence()
        .unwrap();
    for step in steps.iter().filter(|step| {
        step["run"]
            .as_str()
            .is_some_and(|run| run.contains("npm test"))
    }) {
        let id = step["id"]
            .as_str()
            .expect("every browser test step needs a recorded ID");
        let condition = step["if"].as_str().unwrap();
        for prerequisite in [
            "!cancelled()",
            "steps.bindings.outcome == 'success'",
            "steps.browsers.outcome == 'success'",
        ] {
            assert!(
                condition.contains(prerequisite),
                "{id} omitted {prerequisite}"
            );
        }
        if id == "web_product" {
            assert!(condition.contains("steps.web_native.outcome == 'success'"));
        }
        assert!(!condition.contains("steps.controllers.outcome"));
        assert!(!condition.contains("success()"));
    }
    let outcomes = steps
        .iter()
        .find(|step| step["name"].as_str() == Some("Record browser step outcomes"))
        .unwrap();
    assert_eq!(outcomes["if"].as_str(), Some("always()"));
    let run = outcomes["run"].as_str().unwrap();
    assert!(run.contains("outcomes.json"));
    assert!(run.contains("results.json"));
    #[cfg(unix)]
    {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir(root.path().join("rsi-web-evidence")).unwrap();
        let execute = || {
            std::process::Command::new("bash").args(["-c",run])
            .env("RUNNER_TEMP",root.path()).env("PRODUCT_OUTCOME","success")
            .env("STEP_OUTCOMES",r#"{"web_product":{"outcome":"success","conclusion":"success","outputs":{"secret":"must-not-copy"}}}"#)
            .output().unwrap()
        };
        assert!(
            !execute().status.success(),
            "an empty report directory is not acceptance evidence"
        );
        fs::write(root.path().join("rsi-web-evidence/results.json"),r#"{"results":[{"browser":"chromium","status":"passed"},{"browser":"firefox","status":"failed"}]}"#).unwrap();
        assert!(
            !execute().status.success(),
            "both browser results must pass"
        );
        fs::write(root.path().join("rsi-web-evidence/results.json"),r#"{"results":[{"browser":"chromium","status":"passed"},{"browser":"firefox","status":"passed"}]}"#).unwrap();
        assert!(execute().status.success());
        let retained =
            fs::read_to_string(root.path().join("rsi-browser-logs/outcomes.json")).unwrap();
        assert!(!retained.contains("must-not-copy"));
        assert!(!retained.contains("outputs"));
    }
}

#[cfg(unix)]
#[test]
fn audit_command_collects_every_lockfile_and_preserves_any_failure() {
    use std::os::unix::fs::PermissionsExt as _;
    let source = fs::read_to_string(repository().join(".github/workflows/ci.yml")).unwrap();
    let workflow: yaml_serde::Value = yaml_serde::from_str(&source).unwrap();
    let step = workflow["jobs"]["dependency-audit"]["steps"]
        .as_sequence()
        .unwrap()
        .iter()
        .find(|step| step["name"].as_str() == Some("Audit every committed Cargo lockfile"))
        .unwrap();
    let root = tempfile::tempdir().unwrap();
    for (name, body) in [
        (
            "git",
            "#!/bin/sh\nprintf 'one/Cargo.lock\\0two/Cargo.lock\\0'\n",
        ),
        (
            "cargo",
            "#!/bin/sh\nprintf '%s\\n' \"$*\" >> \"$RSI_TEST_LOG\"\ncase \"$*\" in *\"$RSI_TEST_FAIL\"*) exit 1;; esac\n",
        ),
    ] {
        let path = root.path().join(name);
        fs::write(&path, body).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
    }
    for (failure, success) in [
        ("./Cargo.lock", false),
        ("one/Cargo.lock", false),
        ("never-match", true),
    ] {
        let log = root.path().join("calls");
        fs::write(&log, "").unwrap();
        let output = std::process::Command::new("bash")
            .args(["-e", "-o", "pipefail", "-c", step["run"].as_str().unwrap()])
            .env(
                "PATH",
                format!(
                    "{}:{}",
                    root.path().display(),
                    std::env::var("PATH").unwrap()
                ),
            )
            .env("RSI_TEST_LOG", &log)
            .env("RSI_TEST_FAIL", failure)
            .output()
            .unwrap();
        assert_eq!(output.status.success(), success);
        assert_eq!(
            fs::read_to_string(log).unwrap().lines().collect::<Vec<_>>(),
            [
                "audit --file ./Cargo.lock",
                "audit --no-fetch --file one/Cargo.lock",
                "audit --no-fetch --file two/Cargo.lock",
            ]
        );
    }
}

#[test]
fn ci_baseline_script_fetches_exact_nonancestor_commits_and_rejects_missing_input() {
    let source = fs::read_to_string(repository().join(".github/workflows/ci.yml")).unwrap();
    let workflow: yaml_serde::Value = yaml_serde::from_str(&source).unwrap();
    let steps = workflow["jobs"]["documentation"]["steps"]
        .as_sequence()
        .unwrap();
    let script = steps
        .iter()
        .find(|step| step["id"].as_str() == Some("notes_base"))
        .unwrap()["run"]
        .as_str()
        .unwrap();
    let temp = tempfile::tempdir().unwrap();
    let origin = temp.path().join("origin.git");
    let source = temp.path().join("source");
    let checkout = temp.path().join("checkout");
    fs::create_dir(&source).unwrap();
    let git = |cwd: &std::path::Path, args: &[&str]| {
        let output = Command::new("git")
            .args(args)
            .current_dir(cwd)
            .output()
            .unwrap();
        assert!(output.status.success(), "{}", stderr(&output));
        String::from_utf8(output.stdout).unwrap().trim().to_owned()
    };
    git(temp.path(), &["init", "--bare", origin.to_str().unwrap()]);
    git(&source, &["init", "-b", "main"]);
    git(&source, &["config", "user.name", "Fixture"]);
    git(
        &source,
        &["config", "user.email", "fixture@example.invalid"],
    );
    git(&source, &["commit", "--allow-empty", "-m", "initial"]);
    let initial = git(&source, &["rev-parse", "HEAD"]);
    git(
        &source,
        &["remote", "add", "origin", origin.to_str().unwrap()],
    );
    git(&source, &["push", "origin", "main"]);
    git(
        temp.path(),
        &[
            "clone",
            "--single-branch",
            "-b",
            "main",
            origin.to_str().unwrap(),
            checkout.to_str().unwrap(),
        ],
    );
    git(&source, &["checkout", "--orphan", "previous"]);
    git(
        &source,
        &["commit", "--allow-empty", "-m", "force predecessor"],
    );
    let previous = git(&source, &["rev-parse", "HEAD"]);
    git(&source, &["push", "origin", "previous"]);
    let run = |event: &str, sha: &str, succeeds: bool| {
        let receipt = temp.path().join("output");
        fs::write(&receipt, "").unwrap();
        let output = Command::new("python3")
            .args(["-c", script])
            .current_dir(&checkout)
            .env("NOTES_EVENT", event)
            .env("NOTES_PR_BASE", sha)
            .env("NOTES_PUSH_BEFORE", sha)
            .env("GITHUB_OUTPUT", &receipt)
            .output()
            .unwrap();
        assert_eq!(output.status.success(), succeeds, "{}", stderr(&output));
        fs::read_to_string(receipt).unwrap()
    };
    assert_eq!(
        run("pull_request", &initial, true),
        format!("base={initial}\n")
    );
    assert_eq!(run("push", &previous, true), format!("base={previous}\n"));
    assert_eq!(
        run("workflow_dispatch", "", true),
        format!("base={initial}\n")
    );
    assert_eq!(
        run("push", &"0".repeat(40), true),
        format!("base={}\n", "0".repeat(40))
    );
    for (event, sha) in [
        ("push", ""),
        ("pull_request", ""),
        ("push", "1111111111111111111111111111111111111111"),
        ("pull_request", "0000000000000000000000000000000000000000"),
    ] {
        assert!(run(event, sha, false).is_empty());
    }
}

#[test]
fn ci_guard_step_references_resolve_to_unique_steps() {
    let source = fs::read_to_string(repository().join(".github/workflows/ci.yml")).unwrap();
    let workflow: yaml_serde::Value = yaml_serde::from_str(&source).unwrap();
    assert!(ci_guard_step_errors(&workflow).is_empty());
    for (job, id) in [
        ("rsi-ssh-linux", "ssh_setup"),
        ("rsi-ssh-linux", "ssh_prerequisites"),
        ("rsi-ssh-linux", "ssh_helper_build"),
        ("rsi-ssh-linux", "ssh_native"),
        ("rsi-base", "base_setup"),
    ] {
        for replacement in [None, Some("renamed")] {
            let mut broken = workflow.clone();
            let steps = broken["jobs"][job]["steps"].as_sequence_mut().unwrap();
            let step = steps
                .iter_mut()
                .find(|step| step["id"].as_str() == Some(id))
                .unwrap();
            step["id"] = replacement.map_or(yaml_serde::Value::Null, Into::into);
            assert!(!ci_guard_step_errors(&broken).is_empty(), "{job}: {id}");
        }
    }
    let mut duplicate = workflow.clone();
    let steps = duplicate["jobs"]["rsi-base"]["steps"]
        .as_sequence_mut()
        .unwrap();
    steps[0]["id"] = "base_setup".into();
    assert!(!ci_guard_step_errors(&duplicate).is_empty());
}

fn ci_guard_step_errors(workflow: &yaml_serde::Value) -> Vec<String> {
    let mut errors = Vec::new();
    for (job, value) in workflow["jobs"].as_mapping().unwrap() {
        let Some(steps) = value["steps"].as_sequence() else {
            continue;
        };
        let mut ids = std::collections::BTreeSet::new();
        for id in steps.iter().filter_map(|step| step["id"].as_str()) {
            if !ids.insert(id) {
                errors.push(format!("{job:?}: duplicate step {id}"));
            }
        }
        for condition in steps.iter().filter_map(|step| step["if"].as_str()) {
            for reference in condition.split("steps.").skip(1) {
                let id = reference.split('.').next().unwrap();
                if !ids.contains(id) {
                    errors.push(format!("{job:?}: unresolved guard step {id}"));
                }
            }
        }
    }
    errors
}

#[test]
fn native_verification_depends_on_toolchain_not_cache_success() {
    let source = fs::read_to_string(repository().join(".github/workflows/ci.yml")).unwrap();
    let workflow: yaml_serde::Value = yaml_serde::from_str(&source).unwrap();
    for job in workflow["jobs"].as_mapping().unwrap().values() {
        let Some(steps) = job["steps"].as_sequence() else {
            continue;
        };
        for id in ["docs_setup", "base_setup", "native_setup"] {
            let Some(setup) = steps.iter().find(|step| step["id"].as_str() == Some(id)) else {
                continue;
            };
            assert!(
                setup["uses"]
                    .as_str()
                    .unwrap()
                    .starts_with("dtolnay/rust-toolchain@")
            );
            // A skipped consumer must not erase a prerequisite failure from the job result.
            assert!(job["continue-on-error"].is_null());
            assert!(setup["continue-on-error"].is_null());
            if id == "docs_setup" {
                continue;
            }
            let cache = steps
                .iter()
                .find(|step| {
                    step["uses"]
                        .as_str()
                        .is_some_and(|uses| uses.starts_with("Swatinem/rust-cache@"))
                })
                .unwrap();
            assert!(cache["id"].is_null());
            assert!(cache["continue-on-error"].is_null());
            for step in steps.iter().filter(|step| {
                step["if"]
                    .as_str()
                    .is_some_and(|condition| condition.contains(id))
            }) {
                assert!(step["if"].as_str().unwrap().contains("!cancelled()"));
            }
        }
    }
}

#[test]
fn ssh_native_prerequisites_have_owned_ids_guards_and_timeouts() {
    let source = fs::read_to_string(repository().join(".github/workflows/ci.yml")).unwrap();
    let workflow: yaml_serde::Value = yaml_serde::from_str(&source).unwrap();
    let steps = workflow["jobs"]["rsi-ssh-linux"]["steps"]
        .as_sequence()
        .unwrap();
    for (name, id) in [
        (
            "Install isolated Linux SSH prerequisites",
            "ssh_prerequisites",
        ),
        ("Build receipt-compatible static helper", "ssh_helper_build"),
        ("Verify native manager and namespace support", "ssh_native"),
    ] {
        let step = steps
            .iter()
            .find(|step| step["name"].as_str() == Some(name))
            .unwrap();
        assert_eq!(step["id"].as_str(), Some(id));
    }
    let setup = steps
        .iter()
        .find(|step| step["id"].as_str() == Some("ssh_setup"))
        .unwrap();
    assert!(
        setup["uses"]
            .as_str()
            .unwrap()
            .starts_with("dtolnay/rust-toolchain@")
    );
    assert_eq!(setup["with"]["toolchain"].as_str(), Some("1.97.0"));
    for step in steps.iter().take(4) {
        assert!(step["timeout-minutes"].as_u64().is_some());
    }
    let native = steps
        .iter()
        .find(|step| step["id"].as_str() == Some("ssh_native"))
        .unwrap();
    assert!(native["timeout-minutes"].as_u64().is_some());
    assert!(native["continue-on-error"].is_null());
    let condition = native["if"].as_str().unwrap();
    assert!(condition.contains("!cancelled()"));
    assert!(!condition.contains("ssh_helper_build"));
    for prerequisite in ["ssh_setup", "ssh_prerequisites"] {
        assert!(condition.contains(&format!("steps.{prerequisite}.outcome == 'success'")));
    }
}

#[test]
fn ssh_native_checks_report_independent_failures_after_required_setup() {
    let source = fs::read_to_string(repository().join(".github/workflows/ci.yml")).unwrap();
    let workflow: yaml_serde::Value = yaml_serde::from_str(&source).unwrap();
    let job = &workflow["jobs"]["rsi-ssh-linux"];
    let steps = job["steps"].as_sequence().unwrap();
    for name in [
        "Install isolated Linux SSH prerequisites",
        "Lint Execution and SSH packages",
        "Test Execution and SSH packages",
        "Build receipt-compatible static helper",
    ] {
        let step = steps
            .iter()
            .find(|step| step["name"].as_str() == Some(name))
            .unwrap();
        assert!(
            step["timeout-minutes"].as_u64().is_some(),
            "unbounded SSH prerequisite: {name}"
        );
    }
    let position = |name| {
        steps
            .iter()
            .position(|step| step["name"].as_str() == Some(name))
            .unwrap()
    };
    assert!(
        position("Verify SSH cache on the default stack")
            < position("Verify target native lifecycle")
    );
    let cache = steps[position("Verify SSH cache on the default stack")]["run"]
        .as_str()
        .unwrap();
    assert!(cache.contains("env -u RUST_MIN_STACK cargo test --locked -p rsi-ssh-helper --lib --test cache -- --ignored"));
    assert!(
        !steps[position("Verify cache and execution")]["run"]
            .as_str()
            .unwrap()
            .contains("--test cache")
    );
    for name in [
        "Verify generated OpenSSH policy and trust",
        "Verify target native lifecycle",
        "Verify cache and execution",
        "Verify watchdog and cgroup cleanup",
        "Verify source-reader sandbox",
        "Verify actual SSH execution, context, LSP, MCP and saturated control ACKs",
        "Verify SSH cache on the default stack",
    ] {
        let step = job["steps"]
            .as_sequence()
            .unwrap()
            .iter()
            .find(|step| step["name"].as_str() == Some(name))
            .unwrap();
        let condition = step["if"].as_str().unwrap();
        assert!(condition.contains("!cancelled()"));
        for prerequisite in ["ssh_setup", "ssh_helper_build", "ssh_native"] {
            assert!(condition.contains(&format!("steps.{prerequisite}.outcome == 'success'")));
        }
        assert!(step["continue-on-error"].is_null());
        assert!(step["timeout-minutes"].as_u64().is_some());
        assert_eq!(
            step["run"].as_str().unwrap().matches("cargo test").count(),
            1
        );
    }
    assert!(job["continue-on-error"].is_null());
}

#[test]
fn uds_test_support_is_checked_independently_of_base_test_outcomes() {
    let source = fs::read_to_string(repository().join(".github/workflows/ci.yml")).unwrap();
    let workflow: yaml_serde::Value = yaml_serde::from_str(&source).unwrap();
    let setup = workflow["jobs"]["rsi-base"]["steps"]
        .as_sequence()
        .unwrap()
        .iter()
        .find(|step| step["id"].as_str() == Some("base_setup"))
        .unwrap();
    assert!(
        setup["uses"]
            .as_str()
            .unwrap()
            .starts_with("dtolnay/rust-toolchain@")
    );
    assert_eq!(setup["with"]["toolchain"].as_str(), Some("1.97.0"));
    let step = workflow["jobs"]["rsi-base"]["steps"]
        .as_sequence()
        .unwrap()
        .iter()
        .find(|step| step["name"].as_str() == Some("Check UDS test-support contracts"))
        .unwrap();
    let condition = step["if"].as_str().unwrap();
    assert!(condition.contains("!cancelled()"));
    assert!(condition.contains("steps.base_setup.outcome == 'success'"));
    assert!(condition.contains("runner.os == 'Linux'"));
    let run = step["run"].as_str().unwrap();
    assert!(run.contains(
        "cargo clippy --locked -p rsi-api-uds-client --features test-support --all-targets"
    ));
    assert!(run.contains(
        "cargo test --locked -p rsi-api-uds-client --features test-support --test client"
    ));
    assert!(!run.contains("--ignored"));
}
