# rsi-app-tools

Application development supervision and paired distribution consume the current
working tree, including dirty and untracked source, without changing the Git index.
Commands resolve tooling from the validated current repository root, including
after the checkout or executable has moved.
`dev tui` retains isolated HOME/XDG state, foreground terminal ownership and native
renderer watching. The frontend invokes `dev web`; its Vite document source overlay
is explicitly outside the immutable bootstrap claim, while the upstream native and
Worker stay paired. Provider secrets are not inherited by build or default dev.

`dist web` publishes rsi, assets and a same-CPU Linux musl SSH helper; `dist desktop`
also publishes the Desktop executable and its rsi companion. The helper always
uses the release profile, even when the main application selects debug. Published
Linux executables have mode 0755 independent of the build artifact mode. The build
family records its target, compiler identity and profile; the distribution receipt
binds its exact SHA-256 alongside the native and Web artifacts. Missing musl tooling
fails publication without replacing the current generation.
An optional new absolute output directory is
caller-owned. Without an output, managed generations live under target/rsi-app.
`--debug` selects a debug build. Source capture is serialized at one stable path,
with shared Cargo and pnpm caches. Capture resolves the checkout root before
checking relative symlink containment; captured symlinks retain their literal targets.
Frozen bytes, symlinks and read-only file modes
are checked before and after compilation. Source capture checks file identity,
content timestamps and mode; access-time changes caused by concurrent readers do
not invalidate unchanged input. A failed build never replaces current.

`gc` retains current, the previous successful generation, the newest failed
build diagnostic and every locked live generation. Successful publication and failed managed builds also
run collection under the build lock; a cleanup failure is reported without
hiding the original build failure. Running native processes pin managed publications, including a
daemon after its launcher exits. Development supervisors own external publications
for the lifetime of their processes and watchers. No full build tree is copied to /tmp.


## Development supervision

From the repository root, `cargo run --locked -p rsi-app-tools -- dev tui|web`
creates a private development environment with a deterministic native provider.
Linux/WSL is required. `--directory ABSOLUTE_NEW_DIRECTORY` selects persistent
state and rejects an existing destination. `--prepare-only` builds/configures and
retains the environment without starting an application. `--smoke` instead runs a
keyless headless request and checks its provider response and durable completion;
it cannot be combined with `--prepare-only`. `--no-watch` disables source watching.
`--port PORT` selects a nonzero Web frontend port (default 8787) and is rejected
for TUI. Unknown, missing and duplicate options fail before setup.

Default development environments live below `target/rsi-app/dev`. A short private
runtime directory lives separately in `/tmp` for Unix socket limits; it contains
runtime state, not a copied source/build tree. Successful default runs remove the
environment, runtime directory and private native artifacts; failed runs preserve
them for diagnosis. Explicit directories and prepare-only runs persist, with
`runtime-path` and `native-output-path` recording their associated directories.

Product children receive a cleared environment containing only terminal/locale
inputs and isolated HOME/XDG paths. Build children additionally receive explicit
Cargo/Rustup homes, caches and selected toolchain inputs, including Cargo's
incremental, job-count and development/test debug-info settings; these homes can contain
registry credentials. Generated `run` launchers omit build-only variables. This
is configuration isolation, not a filesystem sandbox. Native builds select the
launcher target explicitly; native renderer compilation/copying is serialized
under `target/dev-native` using Linux `flock`, with shared caches and private publication paths.

Web supervision first builds the paired upstream, then starts Vite and an isolated
API listener at separate loopback ports. Vite forwards only the intended API and
paired Worker assets; browser source access is bounded by the Vite configuration.
React feature changes support HMR; bootstrap changes reload the document. Rust or
Worker changes require a new paired upstream and restart. Renderer publication
continues through its independent generation owner. [The Web contract](../web/README.md)
owns the deliberate development source-overlay exception.

Each supervised application/watcher owns a process group. TUI receives foreground
terminal ownership, and cleanup restores terminal modes and the prior foreground
group. Shutdown signals and drains children, allowing 15 seconds after TERM and
two seconds after KILL. A child that cannot be reaped is reported as incomplete
cleanup and retains the environment. Builds and default runs never load provider
keys from the developer's environment or `.local/dev/.env`.

Paired publication requires Linux or WSL, Python 3.11+, the repository's pinned
Rust toolchain with `wasm32-unknown-unknown` and the same-CPU Linux musl target,
`musl-gcc` (or an explicit absolute `RSI_MUSL_CC`), Node 22+, pnpm 12.6.0, and
`wasm-bindgen-cli` 0.2.127 on PATH (or `RSI_WASM_BINDGEN`). Desktop publication
also requires `pkg-config` and GTK 3 / WebKitGTK 4.1 development libraries.
The tools validate and record their versions in the build family; they do not
install these external prerequisites automatically.
