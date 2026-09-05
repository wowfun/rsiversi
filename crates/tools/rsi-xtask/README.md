# rsi-xtask

`rsi-xtask` is the private command-line tool for repository policy and cross-workspace verification orchestration. It is invoked through the root Cargo alias as `cargo xtask`; its checks do not edit tracked files unless a caller explicitly selects a documented `--write` mode.

## Documentation policy

`cargo xtask verify-docs` validates repository-root execution, documentation layout, governance boundaries, active `AGENTS.md` word budgets, Cargo package README identity and minimum prose, internal Markdown links, and active Agent Notes. Independent diagnostics are collected and printed in stable path, line, and message order.

## Agent Note archives

`cargo xtask verify-agent-notes` runs the focused Note lifecycle and archive-integrity checks. `cargo xtask verify-agent-notes --write` is the only documentation command that may append archive seals; it never edits or replaces an existing sealed entry.

## Optional code checks

`cargo xtask code-check` runs the repository checks configured by
[`code-check.toml`](code-check.toml) when a contributor invokes it explicitly.
It is not part of CI, conformance, documentation verification, or another
required gate.

The current source-structure check parses every tracked or non-ignored
untracked regular Rust source file, including tests and standalone fixtures.
Blank and comment-only lines do not count. Files above the configured line
threshold produce warnings in descending effective-line-count order, with
repository-relative paths ascending as the deterministic tie-breaker. Each
warning identifies up to three largest direct top-level items, the largest
named function or method, and the named function or method with the deepest
control flow. These findings do not fail the command.

Analysis covers source as written, including inactive `cfg` branches, without
expanding macros or resolving names. Invalid configuration, source enumeration,
reads, or Rust syntax remain execution errors. Source errors are collected in
stable path, line, column, and message order, and prevent partial findings or a
success summary from being printed.

## rsi-meta verification

`cargo xtask rsi-meta conformance` is the single CI and local orchestration
authority for the foundation. It runs locked, warning-denied Clippy and
all-target tests for the runtime-independent contract, core,
`rsi-meta-scope`, Profile, ABI, and Loader. It also validates
both standalone fixture manifests offline, formats
and lints them, tests and release-builds `echo-bidi`, and either runs the
release `foundation-probe` on Linux or release-builds it on other hosts. On
Linux it additionally inspects the built ELF dynamic symbol table and accepts
only the v3 plugin entry export. The inspection uses `NM` when set and otherwise
uses `nm`, so cross-toolchain Linux hosts can select the matching GNU- or
LLVM-compatible inspector. The ABI package test owns the maintained C11/C++17
header compilation, while the Loader suite maps the real native fixture on the
executing host.

Repository commands must run from the repository root. Native evidence applies only to the platform that actually executed it.
