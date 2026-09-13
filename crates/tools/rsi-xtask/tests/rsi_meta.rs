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
    let metadata: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("cargo metadata JSON");
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
        "rsi-ai",
        "rsi-agent",
        "rsi",
        "rsi-desktop",
        "repository-tools",
    ];
    let job_patterns = package_jobs
        .into_iter()
        .map(|job_name| {
            let patterns = workflow.jobs[job_name]
                .steps
                .iter()
                .filter_map(|step| step.run.as_deref())
                .flat_map(cargo_package_patterns)
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
        assert_eq!(
            components
                .next()
                .and_then(|value| value.as_os_str().to_str()),
            Some("crates"),
            "workspace package {name} escaped crates/"
        );
        let product = components
            .next()
            .and_then(|value| value.as_os_str().to_str())
            .expect("product directory");
        if product == "rsi-meta" {
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
            "workspace package {name} at {} is selected by CI jobs {owners:?}",
            relative.display()
        );
    }
}

fn cargo_package_patterns(command: &str) -> Vec<String> {
    command
        .lines()
        .flat_map(|line| {
            let tokens = line.split_whitespace().collect::<Vec<_>>();
            if !tokens.contains(&"--all-targets") {
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
    assert!(cargo_package_patterns(preflight).is_empty());
    assert_eq!(
        cargo_package_patterns(&format!(
            "{preflight}\ncargo clippy --locked -p rsi-desktop --all-targets -- -D warnings"
        )),
        vec!["rsi-desktop"]
    );
}

fn package_matches(pattern: &str, package: &str) -> bool {
    pattern
        .strip_suffix('*')
        .map_or_else(|| package == pattern, |prefix| package.starts_with(prefix))
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
    let result_variables = contract
        .env
        .iter()
        .filter_map(|(variable, expression)| {
            let job = expression
                .strip_prefix("${{ needs.")?
                .strip_suffix(".result }}")?;
            Some((job, variable.as_str()))
        })
        .collect::<BTreeMap<_, _>>();
    assert_eq!(
        result_variables.keys().copied().collect::<BTreeSet<_>>(),
        expected,
        "ci-required does not observe every needed result"
    );
    let run = contract.run.as_deref().expect("aggregate contract script");
    for variable in result_variables.values() {
        assert!(
            run.contains(&format!("test \"${variable}\" = success")),
            "ci-required does not require `{variable}` to succeed"
        );
    }
}

#[test]
fn ci_frontend_smoke_retains_the_required_sandbox_policy_until_exit() {
    let source = fs::read_to_string(repository().join(".github/workflows/ci.yml")).unwrap();
    let workflow: yaml_serde::Value = yaml_serde::from_str(&source).unwrap();
    let steps = workflow["jobs"]["rsi"]["steps"].as_sequence().unwrap();
    let step = steps
        .iter()
        .find(|step| {
            step["run"]
                .as_str()
                .is_some_and(|run| run.contains("cargo xtask dev tui --smoke"))
        })
        .expect("product CI exercises the actual development launcher");
    assert_eq!(step["if"].as_str(), Some("runner.os == 'Linux'"));
    let run = step["run"].as_str().unwrap();
    let smoke = run.find("cargo xtask dev tui --smoke").unwrap();
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
fn ci_job_deadlines_cover_their_explicit_step_budgets_with_headroom() {
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

    let workflow = fs::read_to_string(repository().join(".github/workflows/ci.yml")).unwrap();
    let workflow: Workflow = yaml_serde::from_str(&workflow).expect("workflow YAML");
    for (name, job) in workflow.jobs {
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
        "npm run typecheck --prefix ../../../plugins/rsi/web",
        "npm test --prefix ../../../plugins/rsi/web",
    ] {
        assert!(
            browser.contains(command),
            "document check omitted: {command}"
        );
    }
    let desktop = scripts("rsi-desktop");
    for seam in [
        "fixtures/rsi/desktop-admission/Cargo.toml",
        "--foreign-binary",
        "--ack-timeout",
        "--startup-close",
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
