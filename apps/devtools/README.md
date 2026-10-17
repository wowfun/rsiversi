# rsi-app-tools

Application development supervision and paired distribution consume the current
working tree, including dirty and untracked source, without changing the Git index.
Commands resolve tooling from the validated current repository root, including
after the checkout or executable has moved.
`dev tui` retains isolated HOME/XDG state, foreground terminal ownership and native
renderer watching. The frontend invokes `dev web`; its Vite document source overlay
is explicitly outside the immutable bootstrap claim, while the upstream native and
Worker stay paired. Provider secrets are not inherited by build or default dev.

The source entrypoints `rsi.sh` and `rsi.ps1` own prerequisite diagnosis, explicit
setup and direct published launch. The Rust builder owns all publication; Python
is not a source-install prerequisite. `build` defaults to release; `--debug` is
explicit. Windows requires Windows 11 24H2+ x64 and an MSVC/SDK toolchain.
Windows build runs a private copy of the builder so concurrent Cargo invocations
can relink the shared tool. Publication itself remains serialized by its lock.
Missing optional installation-directory environment variables do not suppress
other prerequisite diagnostics. Private-runner cleanup is best effort and cannot
replace the build result. Linux Web prerequisites include both the WebAssembly
and same-CPU musl Rust targets; `pkg-config` belongs to Desktop preparation.

`dist web` publishes rsi and assets on Linux and Windows, plus a same-CPU Linux musl SSH helper on Linux; `dist desktop`
also publishes the Desktop executable and its rsi companion. The helper always
uses the release profile, even when the main application selects debug. Published
Linux executables have mode 0755 independent of the build artifact mode. The build
family records its target, compiler identity and profile; the distribution receipt
binds its exact SHA-256 alongside the native and Web artifacts. Missing musl tooling
fails publication without replacing the current generation.
An optional new absolute output directory is caller-owned and must resolve outside
`target/rsi-app`, including through an existing directory alias. Parent traversal
is rejected. The builder checks containment before and after claiming the directory
and uses its canonical path for subsequent writes. The final directory is created
exclusively. A detected path change aborts before artifact/report writes; any empty
directory created during the race is left for inspection. Checkout, cache and
output ancestors are caller-controlled directories, not a sandbox against another
process continuously replacing paths. Without an output, managed generations live
under target/rsi-app.
`--debug` selects a debug build. Source capture is serialized at one stable path,
with shared Cargo and pnpm caches. Capture resolves the checkout root before
checking relative symlink containment; captured symlinks retain their literal targets.
Frozen bytes, symlinks and read-only file modes
are checked before and after compilation. Source capture checks bytes, content
timestamps and mode, plus device/inode identity on Unix and creation time and
attributes on Windows. Missing tracked files represent working-tree deletions;
other metadata errors abort capture. Access-time changes caused by concurrent
readers do not invalidate unchanged input. Only `apps/web/node_modules` and
`apps/desktop/gen` are reserved generated directories in a capture; authored
inputs there are rejected. Other directories, including nested directories named
`target` or `node_modules`, remain subject to uncaptured-addition checks.
Each regular source file is limited to 64 MiB, including growth during capture;
put large generated artifacts in Git-ignored paths. Capture, comparison and
digest reads use fixed streaming buffers, and unchanged captured files retain
their timestamps. A failed build or publication
never replaces current; bounded failure categories distinguish compilation,
publication and panics. A caught panic retains evidence without automatic collection;
the next explicit GC or ordinary build revalidates the durable cache. Failure-evidence writes and cleanup are best effort and
cannot replace its diagnostic. Once current is published, collection failure is
reported separately and cannot turn that successful publication into a build failure.
Paired build children receive `CI=true`, including local debug publications.

The ASCII `target/rsi-app/current` file contains exactly 20 decimal digits followed
by one LF byte (21 bytes total). The identifier is Unix-epoch nanoseconds padded
with leading zeroes; values exceeding 20 digits are rejected before creating output.
The Rust publisher opens the final component without following links and validates
the opened handle as an ordinary file before reading. It never
contains a path or executable command. This grammar also governs generation
directory and lease names. GC scans and rejects unexpected generation or trash directory
shapes before removing any generation; errors identify unexpected trash paths for
inspection and manual relocation outside the cache. Unknown entries are never
automatically deleted. Fixed-width identifiers sort chronologically. All
readers resolve it below `generations/<id>/bundle`. Publication atomically replaces
this record with bounded retries, retaining the previous successful version on
failure. Receipt format 2 explicitly permits a missing SSH helper on Windows.
Managed binaries require a lease at `leases/<generation-id>` outside their generation
directory and acquire it without waiting. The compiled manifest identifies managed output;
missing or contested leases fail before assets or application state are opened.
Shell selection is not a lease: a launch paused across publication changes can
lose its selected generation before native startup acquires the shared lock.
It then fails closed; rerun the source entrypoint to select current. A lease pins
the generation only after acquisition and a post-lock existence check, on both
Linux and Windows. Merely opening an executable is not the lifetime contract.
Collection locks a generation exclusively and renames it into trash before
removing files. Later collection retries abandoned trash even when no generations
directory remains, and retries orphan lease removal;
it never removes a lease still held by a live process. Windows source captures retain writable file attributes while
hash/metadata checks detect capture changes; Unix captures retain read-only modes.

`gc` retains current, the previous successful generation, the newest failed
build diagnostic and every locked live generation. A failure marker takes
precedence over a completed receipt when publication fails. Successful publication and ordinary managed build failures also
run collection under the build lock. Explicit `gc` and addon builds wait for that
same owner, which may be a long release build; the waiting message precedes the lock.
A cleanup failure is reported without
hiding the original build failure. Running native processes pin managed publications, including a
daemon after its launcher exits. Development supervisors own external publications
for the lifetime of their processes and watchers. No full build tree is copied to /tmp.


## Development supervision

From the repository root, `cargo run --locked -p rsi-app-tools -- dev tui|web`
creates a private development environment with a deterministic native provider.
Linux/WSL is required. `--directory ABSOLUTE_NEW_DIRECTORY` selects persistent
state and rejects an existing destination. `--prepare-only` builds/configures and
retains the environment without starting an application. `--smoke` instead runs a
keyless headless request and decodes its complete settled model attempt to check
the provider response and matching durable completion;
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

Paired publication requires Linux, WSL, or native Windows, the repository's pinned
Rust toolchain with `wasm32-unknown-unknown`, Node 22.18+, pnpm 12.6.0, and
`wasm-bindgen-cli` 0.2.127 on PATH (or `RSI_WASM_BINDGEN`). Desktop publication
also requires `pkg-config` and GTK 3 / WebKitGTK 4.1 development libraries.
Linux publication additionally needs the same-CPU musl Rust target and `musl-gcc`
(or an explicit absolute `RSI_MUSL_CC`); Windows does not build an SSH helper.
The Rust publisher validates and records versions in the build family. Doctor
reports an absent pinned Rust toolchain with the setup command and collects
Git-checkout and other prerequisite failures on both PowerShell versions. Shell
setup prepares user tools; installing system prerequisites remains explicit.
The Windows `bootstrap` integration test additionally requires PowerShell 7 on
PATH to exercise both supported PowerShell implementations, alongside inbox
Windows PowerShell, Git for Windows and Rust. Missing test tools fail with a named
prerequisite diagnostic; this does not add PowerShell 7 to installation requirements.

`dist addon-workbench ABSOLUTE_NEW_REPORT` is the Linux fixture entrypoint. It
builds the independent example from the current verified capture under the build
lock, retains that publication's lease during browser verification, and records
the example digest separately. Precondition failures leave the requested report
path unused. Once work starts, a failure retains logs and a bounded `failure.json`
stage; `fixture-receipt.json` is written only after browser verification succeeds.
An existing report directory is refused with guidance to choose a new path.
It shares capture validation with publication.
