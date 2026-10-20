# rsi-app-tools

Application development supervision and paired distribution consume the current
working tree, including dirty and untracked source, without changing the Git index.
Commands resolve tooling from the validated current repository root, including
after the checkout or executable has moved.
`dev tui|web` starts ordinary debug applications with caller-selected state and
credentials. `fixture-dev tui|web` retains deterministic isolated Linux/WSL
layout and smoke workflows. Build and watcher subprocesses use filtered inputs;
product subprocesses retain the caller's normal runtime environment.

The source entrypoints `scripts/rsiversi.sh` and `scripts/rsiversi.ps1` own prerequisite diagnosis, explicit
setup and direct published launch. The Rust builder owns all publication; Python
is not a source-install prerequisite. `build` defaults to release; `--debug` is
explicit. The default source target is `tui`; `web` and `desktop` are explicit.
Windows requires Windows 11 24H2+ x64 and an MSVC/SDK toolchain.
Windows build runs a private copy of the builder so concurrent Cargo invocations
can relink the shared tool. Publication itself remains serialized by its lock.
Missing optional installation-directory environment variables do not suppress
other prerequisite diagnostics. Private-runner cleanup is best effort and cannot
replace the build result. Linux Web prerequisites include both the WebAssembly
and same-CPU musl Rust targets; `pkg-config` belongs to Desktop preparation.

Entrypoints resolve the repository from their own parent directory, independently
of the caller's working directory. No arguments or a leading `--help`/`-h` print
general help to stdout and exit successfully. For `doctor`, `setup`, `build` and
`path`, either help flag anywhere in the arguments prints command help before
prerequisite checks or side effects, without requiring a publication or build
tools. `web`, `tui` and `desktop` forward their arguments, including help flags, to the
published application under the existing platform/terminal requirements.
Unknown commands fail with guidance to the entrypoint's help. Help text stays
English and the PowerShell script stays ASCII-compatible with PowerShell 5.1.
Published applications inherit the caller's working directory; management tools
run from the repository. Recovery diagnostics name the selected setup target and
any requested SSH helper. Web/Desktop doctor checks npm as well as Node, and an
explicit `RSI_MUSL_CC` must be absolute, matching the builder's admission rule.

## Build targets and launch selection

`doctor`, `setup`, `build` and `path` accept an optional first `tui`, `web` or
`desktop` target, defaulting to `tui`. Build additionally accepts one new absolute
output, `--debug`, `--with-ssh-helper` and `--verbose`. Doctor/setup accept only
the helper option. Unknown and duplicate arguments fail before prerequisites.
Output paths use the host grammar: Linux/WSL accepts `/...`; Windows accepts
drive-rooted paths and UNC paths with either slash spelling. Git Bash converts
its absolute POSIX paths with `cygpath` before passing them to PowerShell.
Drive-relative and ordinary relative paths are rejected. Path syntax admission
does not establish filesystem availability or override publication validation.
TUI prepares/builds the native `rsi` only, keeping the complete catalog, CLI,
Headless, ACP, HTTP API and management commands. It needs no Node, pnpm,
wasm-bindgen, WASM target or graphical libraries. This is a stage selection,
not a Cargo feature split or runtime Profile dependency resolver.

Linux TUI includes the release musl helper only with `--with-ssh-helper`.
Linux Web/Desktop include it by default; that flag is an idempotent confirmation.
Windows rejects an explicit helper request. Without the helper, product SSH
execution is unavailable; ordinary SSH login to run local TUI is unaffected.
System dependencies remain explicit; setup never installs GTK or system runtimes.
Windows Terminal is needed only to run TUI, not to build or display source help.

`tui [--from tui|web|desktop]` and `web [--from web|desktop]` select that target's
current bundle, defaulting to their own target. Only the leading selector is
interpreted; `--` ends source options. Remaining application arguments retain
their boundaries. `desktop` launches the Desktop binary directly. Applications
own their help and resource arguments. Missing publications name the exact build
command; readers never search history or fall back to the legacy global current.
Linux Web and `tui --from web` can share the same compatible Host. Different
families remain incompatible; Windows retains one embedded owner per Host.

The lower-level grammar is `dist tui|web|desktop [ABSOLUTE_OUTPUT] [--debug]
[--with-ssh-helper] [--verbose]`, `gc [tui|web|desktop|all]` (default all), or
`dist addon-workbench ABSOLUTE_NEW_REPORT`. The old `dist gc` alias is not accepted.
The builder's global `--help` and `-h` list its commands, including fixture entrypoints,
without requiring a checkout or build tools.

Each target owns `target/rsi-app/<target>/{current,generations,leases,trash}` and
`target/rsi-app/build/<target>/{source,build-family.json,target}`. One global
build lock serializes capture, publication and GC. Tool downloads and pnpm store
remain shared. A target switch does not rewrite another target's manifest or
capture. Separate first builds and larger caches preserve strict producer identity
without pretending that different toolchains or package selections are compatible.
Receipt format 2 keeps its existing fields and admits `kind=tui`; absent helper
is recorded as null, never inferred merely from a missing file.

Build diagnostics go to stderr: selection, stage, elapsed time, command, log and
result. Waiting/long-running stages report every 15 seconds. `--verbose` mirrors
both child streams to stderr; stdout remains the final JSON result. Failures
include bounded first-error context and a log tail from the failing command only,
including Cargo, Node and pnpm errors; unrecognized failures use that command's tail.
First-error context includes following bytes across read boundaries. Identical
context and tail are printed once; scanning and retained context remain bounded.
Progress output is best effort: a closed stderr cannot abort a build or its cleanup.
Without a jobs environment
override, concurrency is described as Cargo-configured, not an inferred number.

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
under the selected target publication root.
`--debug` selects a debug build. Source capture is serialized at one stable path per target,
with a Cargo cache per target and a shared pnpm store. Capture resolves the checkout root before
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

The ASCII `target/rsi-app/<target>/current` file contains exactly 20 decimal digits followed
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

`gc` visits the selected target, or all three targets, and each retains the two most
recent successful generations (normally current and previous), plus current if it
is older, the newest failed
build diagnostic and every locked live generation. A failure marker takes
precedence over a completed receipt when publication fails. Successful publication and ordinary managed build failures also
run collection under the build lock. Explicit `gc` and addon builds wait for that
same owner, which may be a long release build; the waiting message precedes the lock.
A cleanup failure is reported without
hiding the original build failure. Running native processes pin managed publications, including a
daemon after its launcher exits. Development supervisors own external publications
for the lifetime of their processes and watchers. No full build tree is copied to /tmp.

`fixture-lease ABSOLUTE_BUNDLE` canonicalizes a selected bundle and acquires its
existing generation lease before emitting exactly `ready\n` on stdout. It retains
that lease until stdin reaches EOF; diagnostics use stderr. External distributions
need no GC lease and follow the same handshake. The
[browser fixture owner](../../fixtures/rsi/web-product/README.md) defines its
helper discovery, selection, readiness bounds and cleanup policy.

## Development supervision

Ordinary `dev tui|web` runs the standard product using caller-selected state and
credentials. Source wrappers build and execute a private tool copy outside Cargo's
runtime environment. The [build input policy](build-environment.txt) supplies the
shared bootstrap, builder and watcher allowlist; toolchain homes may contain registry credentials. Direct `cargo run ... dev` is rejected. Debug external
publications live under `target/rsi-dev`; they never publish managed current or
collect managed generations. Product startup prints resolved config/state/cache
roots. Use independent XDG roots for durable-format changes.

Web accepts `--port` (0..65535, default 8787), `--no-open`, and `--no-watch`.
Without watchers it serves the frozen product directly. Watched development
uses bounded child-pipe readiness and exact loopback Origin admission before
releasing the normal login ticket. Renderer writes target a private asset copy.
TUI forwards only `--cwd`, `--resume`, and `--session-id` after `--`.
Reset, Profile, asset and control overrides are not forwarded.

The supervisor supports Linux/WSL process groups and native Windows Job Objects.
The PowerShell development wrapper temporarily disables its script debugger while
waiting for native work, then restores the caller's mode. Ctrl+Break reaches the
native supervisor without leaving the wrapper paused in its debugger.
PowerShell can still return its interrupted-pipeline status after Ctrl+Break,
even when native cleanup succeeds.
Waiting for another publication owner is cancellable before taking its build lock.
Web shutdown closes control/watcher inputs and permits 15 seconds for graceful
exit before forcing settlement for at most 10 seconds. A completed forced
settlement succeeds; an unconfirmed exit is incomplete cleanup. Forced
termination is reported on stderr. TUI and fixture shutdown additionally signal
their process groups. Successful runs remove only their own output after all children
settle; failures retain output. Browser handoff is outside those process owners.
Existing shared Hosts are never restarted automatically.

The build policy contains one ASCII declaration per line, with no blank lines or
comments. A bare environment name passes the caller's value; `NAME=relative/path`
selects a private directory under `target/rsi-app`. Path components contain only
lowercase letters, digits, underscores or hyphens. This data owns the common bootstrap and
builder cache layout. Cargo/Rustup homes default to the caller's toolchain homes
when absent, and build children receive `CI=true`.

After preparing the pinned Web tools, `cargo test --locked -p rsi-app-tools
--test frontend_install -- --ignored` checks repeated offline installs with a
synthetic dependency and private home/store. It verifies the Web package's
explicit lifecycle-script denial without using real product state or services.
Default lifecycle tests execute ordinary supervision with native std-only test
peers on Linux/WSL and Windows, including Job/group cleanup and EOF settlement.
With `RSI_TEST_DEV_BUNDLE` set to a paired debug Web bundle, `cargo test --locked
-p rsi-app-tools --bin rsi-app-tools dev::lifecycle_tests::ordinary_product_runtime_smoke
-- --ignored --exact` additionally starts the real keyless product using private
HOME/XDG roots, checks its public document, then closes the control owner.
The corresponding `ordinary_product_watched_smoke` also requires the pinned Web
tools and live source dependencies. It runs real Vite and the native product,
checks rejection before redeeming a one-use ticket, then checks cookie reuse and
ticket replay through the proxy. Both smokes remain opt-in and keyless; Linux Web
CI runs them against its paired debug publication.

## Fixture development

From the repository root, `cargo run --locked -p rsi-app-tools -- fixture-dev tui|web`
creates a private development environment with a deterministic native provider.
Linux/WSL is required. `--directory ABSOLUTE_NEW_DIRECTORY` selects persistent
state and rejects an existing destination. `--prepare-only` builds/configures and
retains the environment without starting an application. `--smoke` instead runs a
keyless headless request and decodes its complete settled model attempt to check
the provider response and matching durable completion;
it cannot be combined with `--prepare-only`. `--no-watch` disables source watching.
`--port PORT` selects a nonzero Web frontend port (default 8787) and is rejected
for TUI. Unknown, missing and duplicate options fail before setup.

Default fixture environments live below `target/rsi-fixture-dev`, outside the
managed publication cache. An explicit Web fixture directory must also be outside
that cache. A short private
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
cleanup and retains the environment. Fixture builds and runs never load provider
keys from the developer's environment or `.local/dev/.env`.

Paired publication requires Linux, WSL, or native Windows, the repository's pinned
Rust toolchain. Web/Desktop additionally require `wasm32-unknown-unknown`, Node 22.18+, pnpm 12.6.0, and
`wasm-bindgen-cli` 0.2.127 on PATH (or `RSI_WASM_BINDGEN`). Linux Desktop publication
also requires `pkg-config` and GTK 3 / WebKitGTK 4.1 development libraries.
Linux publications that include the SSH helper additionally need the same-CPU musl Rust target and `musl-gcc`
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
Report path confinement is checked before hashing the captured source tree.
It requires a format-2 published Web receipt and a `wasm32-unknown-unknown` worker
target before consuming a report path, reads its immutable manifest, verifies the Web
capture, and replays that manifest's profile, flags and Cargo features. The
`fixture-build profile-leaf-probe ABSOLUTE_NEW_REPORT` CI entrypoint uses the same verified capture and recipe; it returns the binary path and digest in JSON. Other targets cannot
replace this capture. It shares capture validation with publication.
