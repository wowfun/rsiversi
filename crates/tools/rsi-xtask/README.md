# rsi-xtask

`rsi-xtask` is the private command-line tool for repository policy and cross-workspace verification orchestration. It is invoked through the root Cargo alias as `cargo xtask`; its checks do not edit tracked files unless a caller explicitly selects a documented `--write` mode.

## Documentation policy

`cargo xtask verify-docs` validates repository-root execution, documentation layout, governance boundaries, active `AGENTS.md` word budgets, Cargo package README identity and minimum prose, internal Markdown links, and active Agent Notes. Independent diagnostics are collected and printed in stable path, line, and message order.

## Agent Note archives

`cargo xtask verify-agent-notes` runs the focused Note lifecycle and archive-integrity checks. `cargo xtask verify-agent-notes --write` is the only documentation command that may append archive seals; it never edits or replaces an existing sealed entry.

## rsi-meta verification

`cargo xtask rsi-meta code-health` checks the hard per-file production-code
limit and each owned region's recorded maximum. `--write` creates the baseline
or lowers it after a refactor; it refuses automatic increases. A reviewed
baseline increase is allowed when a cohesive ownership boundary genuinely grew
and the contract plus behavior evidence justify keeping it together. The
baseline is a smell detector, not a reason to split one safety state machine
mechanically.

`cargo xtask rsi-meta conformance` is the single CI and local orchestration
authority for the foundation. It checks formatting, then runs locked,
warning-denied Clippy and all-target tests for core, `rsi-meta-scope`, ABI, and
Loader. It also validates both standalone fixture manifests offline, formats
and lints them, tests and release-builds `echo-bidi`, and either runs the
release `foundation-probe` on Linux or release-builds it on other hosts. On
Linux it additionally inspects the built ELF dynamic symbol table and accepts
only the v2 plugin entry export. The inspection uses `NM` when set and otherwise
uses `nm`, so cross-toolchain Linux hosts can select the matching GNU- or
LLVM-compatible inspector. The ABI package test owns the maintained C11/C++17
header compilation, while the Loader suite maps the real native fixture on the
executing host.

Both commands must run from the repository root. Native evidence applies only to the platform that actually executed it.
