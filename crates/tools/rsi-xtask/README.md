# rsi-xtask

`cargo xtask dev tui` and `cargo xtask dev web` create an isolated development
directory, build and copy the real `rsi` executable once, configure a deterministic
native provider, and supervise the selected application. A successful default run
removes its temporary environment, runtime directory and private native outputs.
Failures retain those files for diagnosis. `--directory ABSOLUTE_NEW_DIRECTORY`
selects a persistent environment; `--prepare-only` also retains its environment
without starting an application. Defaults prefer `/var/tmp` and fall back to the
host temporary directory (`TMPDIR`) if it is unavailable. `--port PORT` selects
the Web listener port (default 8787); it is rejected for TUI.
`--smoke` runs a keyless headless request and exits. No provider credential is read
or required by these defaults. Execution requires Linux or WSL; native Windows
and macOS launch are unavailable.

Supervised applications and build subprocesses receive an explicit environment
allowlist and private HOME/XDG directories. Only build subprocesses receive the
developer's Cargo and Rustup homes, including Cargo configuration and registry
credentials. The product and generated `run` launcher omit those variables.
This separates ambient configuration; it is not a filesystem sandbox.
Native builds explicitly select the launcher executable's target triple,
independent of ambient Cargo `build.target`. The initial executable and Web WASM
builds share the repository Cargo target cache.
Native addon builds share `target/dev-native/cache`. The Linux `flock` utility
serializes compilation and copying into private `target/dev-native/artifacts/`
directories, so concurrent watchers cannot exchange artifacts. SourceRoot requires
those copies to remain under the repository. Both compilation caches remain after
exit. For persistent environments, `native-output-path` and `runtime-path` record
the additional directories to remove when the environment is no longer needed.

TUI development selects its independent native presentation Profile. Its existing
addon watcher builds and enables successful artifacts from an explicit SourceRoot;
the application's normal staging owner publishes replacements. Build output goes
to the development log. The watch set is explicit and does not infer a Cargo
dependency graph. `--no-watch` leaves a fixed presentation. Web Worker/bootstrap
changes require rebuilding the Web bundle and restarting its application; renderer
graphs use their separately owned generation publication. The Web application
and source watcher each have a separate process group; the launcher supervises
Ctrl-C shutdown. The TUI also owns a process group, temporarily receives terminal
foreground ownership, and restores terminal modes under the resident owner.
The launcher kills remaining group members before reaping each leader and restores
the previous foreground group when the TUI closes. Cleanup allows 15 seconds
after TERM and a further two seconds after KILL; either deadline failure names
the child and preserves the environment for diagnosis. An OS task that cannot
be killed is reported as incomplete cleanup.

`rsi-xtask` is the private command-line tool for repository policy and cross-workspace verification orchestration. It is invoked through the root Cargo alias as `cargo xtask`; its checks do not edit tracked files unless a caller explicitly selects a documented `--write` mode.

## Documentation policy

`cargo xtask verify-docs` validates repository-root execution, documentation layout, governance boundaries, active `AGENTS.md` word budgets, Cargo package README identity and minimum prose, internal Markdown links, and active Agent Notes. Independent diagnostics are collected and printed in stable path, line, and message order.

Generated build directories and installed `node_modules` are excluded from
documentation traversal; authored fixture documentation is still checked.
Standalone Cargo fixtures must occupy `fixtures/<product>/<fixture>` under an
existing `crates/<product>` namespace and retain their own package README.

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
