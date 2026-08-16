# rsi-xtask

`rsi-xtask` is the private command-line tool for repository policy and cross-workspace verification orchestration. It is invoked through the root Cargo alias as `cargo xtask`; its checks do not edit tracked files unless a caller explicitly selects a documented `--write` mode.

## Documentation policy

`cargo xtask verify-docs` validates repository-root execution, documentation layout, governance boundaries, active `AGENTS.md` word budgets, Cargo package README identity and minimum prose, internal Markdown links, and active Agent Notes. Independent diagnostics are collected and printed in stable path, line, and message order.

## Agent Note archives

`cargo xtask verify-agent-notes` runs the focused Note lifecycle and archive-integrity checks. `cargo xtask verify-agent-notes --write` is the only documentation command that may append archive seals; it never edits or replaces an existing sealed entry.

## rsi-meta verification

`cargo xtask rsi-meta code-health` checks the hard per-file production-code limit and each owned region's non-growing maximum. `--write` creates the baseline or lowers it after a refactor; it refuses increases.

`cargo xtask rsi-meta conformance` discovers every maintained standalone rsi-meta workspace, runs its locked format, Clippy, and test checks, builds and loads the current platform's real plugin artifacts, and finishes with the release demonstration. `cargo xtask rsi-meta release-demo` prepares the default and failpoint daemons plus lifecycle probe, verifies failpoint isolation, and runs only the thirteen-step demonstration.

Both commands must run from the repository root. They support Linux x86_64 and macOS arm64 and write build outputs only below explicit Cargo target directories.
