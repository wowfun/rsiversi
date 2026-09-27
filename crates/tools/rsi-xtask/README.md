# rsi-xtask

`cargo xtask addon new NAME --directory ABSOLUTE_NEW_DIRECTORY` creates an
independent native Tool addon workspace on Linux/WSL. `NAME` is a lowercase ASCII
letter followed by lowercase letters, digits or hyphens, at most 48 bytes.
Generation publishes a private sibling directory atomically and refuses an
existing destination, including a symlink. It neither invokes Cargo nor installs,
enables or edits a preset. The generated README and `agent-profile.toml` show the
explicit build and installation steps and the Portable Tool bridge.

The template is maintained in [addon-template](../../../fixtures/rsi/addon-template/README.md).
Generation copies its lockfile and rewrites only the root package name. SDK
dependencies share the linked template's immutable published Git revision. The
project remains relocatable after the generator checkout is moved or removed. With dependencies cached, the generated
workspace builds with `cargo build --locked --offline` without changing its lock.
The format-2 addon manifest targets the launcher's native OS/architecture and watches its
explicit source files. Native Windows/macOS scaffolding is currently unavailable.

`cargo xtask addon new NAME --directory ABSOLUTE_NEW_DIRECTORY --kind linked`
creates an independent source addon library and its own composition executable.
Its RSI dependencies use one repository URL and one full Git revision, with an
independent lockfile. It uses public Local contracts and existing addon role
catalogs. Source changes take effect when the executable is rebuilt. The project
does not include private modules or fixture source files from this checkout.
The native template remains the default. Delivering dynamic libraries still
requires the existing Native ABI and Portable boundaries.

## Documentation policy

`cargo xtask verify-docs` is read-only and requires the virtual workspace root.
It checks documentation taxonomy, governance boundaries, active `AGENTS.md` word
budgets, Cargo package README identity and minimum prose, relative Markdown links,
and active Agent Notes. Independent diagnostics are reported in stable path,
line and message order. Root instructions have a 400-word budget; descendant
instructions have 300 words. A reasoned, path-specific override is the last resort.

Every Cargo package requires a sibling README with exactly one level-one heading
matching its package name and a nonempty prose paragraph. Application directories
also require a README. Product namespaces own their governance files; root and
collection boundaries must exist. Product docs use the supported cookbook,
postmortem, subsystems and user subdirectories. Standalone Cargo fixtures live
under `fixtures/<product>/<fixture>` in an existing product namespace and retain
their own README. Generated outputs and installed dependencies are excluded;
authored fixture documentation is checked.

`cargo xtask verify-agent-notes` checks Note lifecycle and archive integrity.
Only its explicit `--write` mode may append archive seals; it never replaces an
existing seal. [Agent Notes](../../../.agents/notes/README.md) own that lifecycle.

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

`cargo xtask verify-architecture` rejects normal, build and test dependencies from
reusable `crates/` packages into `apps/`, including maintained standalone Cargo
workspaces and workspace-inherited declarations. It also rejects workspace
patch/replace paths and explicit Cargo target/build source paths into `apps/`.
Workspace patch/replace overrides are checked conservatively even when currently unused, since
they can redirect transitive dependencies. This is a manifest gate: arbitrary
source includes and process invocations remain subject to ownership review, not
a claim of whole-program dependency analysis. CI runs it with documentation
verification. Application development and paired publication belong to
[rsi-app-tools](../../../apps/devtools/README.md).

Architecture verification collects independent filesystem and dependency errors
before returning sorted diagnostics. It follows source-directory symlinks inside
the repository once per physical directory, rejects escapes and reports broken
paths. Its library ownership scope is `crates/`, as required by the repository
architecture; an arbitrary `libs/` tree is not an alternative library root.
The application root is resolved to its physical directory before classifying
edges, including when `apps/` is a symlink. A broken or escaping application root
is an error; sibling names such as `appsfoo/` are not application paths.
