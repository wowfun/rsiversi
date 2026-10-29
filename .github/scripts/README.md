# CI execution contracts

Workflow steps source `standard-packages.sh` and pass its Bash array as quoted
arguments to Cargo. Package patterns are Cargo selectors, not shell globs.

`sandbox-policy.py prepare STATE full|apparmor` records the existing Linux sysctl
values before changing them. The full profile changes only supported userns and
AppArmor keys; the AppArmor profile requires its key. State is created exclusively
with mode 0600, flushed and synchronized before the first host mutation.
`restore STATE` accepts at most 4096 bytes, validates ownership and the exact
supported keys, attempts every recorded restoration, and removes the record only
after success. Missing state is an idempotent no-op; failed restoration retains it.
Scoped Bash steps source `sandbox-policy.sh full|apparmor`. This single owner
installs an EXIT trap before preparation, preserves the command's
failure status, and fails an otherwise successful step when restoration fails.
Desktop keeps its policy through its dependent steps and restores with `always()`.
Ordinary build steps do not acquire sandbox policy.

The slow ACP phase-budget, real Session Browser, and private-Git sandbox tests
remain locally opt-in. Their owning Linux jobs first assert each exact test name
exists, then execute it with `--ignored --exact`; successful zero-test runs fail
the existence gate. Browser tests consume the existing isolated Node, Chromium
and user-manager setup. Native macOS coverage remains a separate CI responsibility.
