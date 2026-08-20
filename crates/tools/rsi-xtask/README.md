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

## rsi-ai verification

`cargo xtask rsi-ai conformance` fetches the maintained provider-plugin
workspace, then runs locked format, offline Clippy, offline tests, and a release
native build for every OpenAI, compatible Chat, DeepSeek, and Xiaomi wrapper.
The resulting artifacts are written to the paths declared by their manifests;
each config schema is compiled, then each artifact is staged, loaded, and driven
through prepare, commit, retire, and shutdown via its exported ABI. No provider
endpoint or real credential is used.

## rsi-agent verification

`cargo xtask rsi-agent conformance` fetches the single rsi-agent fixture workspace's locked dependencies, runs workspace format, offline Clippy, and offline tests, builds all native providers once, then runs the credential-free assembled scenario offline. A cold Cargo cache may therefore require network access only during the explicit fetch step; the runtime scenario remains keyless and does not contact a live service. The runner loads real native scripted-model and echo-tool providers through `CompositionHost`, exercises all five `rsi-ai` services and artifact bytes, verifies two durable language/tool transcripts, and reopens them to prove replay performs no additional service work.

The command must run from the repository root. Its native conformance claim covers Linux x86_64 and macOS arm64; Cargo outputs stay within fixture-controlled target directories.
