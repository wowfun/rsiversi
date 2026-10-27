# rsi-app-tools

The source installers reject explicitly empty binary or prefix arguments before
build or filesystem work. Windows builds inherit the caller's output handles;
heartbeat messages remain visible, and interrupted waits terminate the owned
build process tree before disposing the process handle.
The `scripts/install.sh` and `scripts/install.ps1` first-install entrypoints install
the native `rsi`, independent uninstaller and platform uninstall launcher, and
configure the user's PATH through the versioned native installation operation.
The [installation owner](../../crates/rsi/installation/README.md) defines records,
publication, PATH receipts and removal authority. They do not select applications, change homes, install
plugins or run the product. An existing installation is upgraded through the
core transaction, rather than overwritten by the bootstrap installer.
After parsing arguments and resolving the destination, installers reject an
existing destination (including directories and dangling links) or a non-directory
parent before discovering Cargo, preparing build directories, inspecting the
source executable or changing PATH. These checks do not execute an existing
installation. Refusals name the absolute conflicting path and offer another
prefix; an ordinary existing executable also receives a shell-quoted command
targeting that exact installation with `core upgrade --local`. Its argument is
a core package directory containing `package.json`, not a bare executable.
The [source installation tutorial](../../docs/user/source-install.md) explains
package preparation. Installers do not add a package producer dependency.
`--binary` / `-Binary` requires `--uninstaller` / `-Uninstaller` and installs an
explicitly trusted prebuilt pair without Cargo;
`--prefix` / `-Prefix` chooses the installation root, and `--no-path` / `-NoPath`
supports isolated installation verification.
The Bash entrypoint supports Linux/WSL and Windows Git Bash (`MINGW*`/`MSYS*`).
On Git Bash it converts explicit paths with `cygpath` and delegates to the adjacent
Windows PowerShell installer, preserving argument boundaries and its exit status.
Bash argument parsing uses exit 2; PowerShell refusals use exit 1. Callers test
for nonzero status, while delegated execution preserves the child's exact status.
Windows defaults to `%LOCALAPPDATA%\RSIversi`, including when invoked from Git Bash;
Linux/WSL defaults to `$HOME/.local`. Windows PATH setup updates the user environment,
not Bash startup files. Help requires neither PowerShell nor build tools.
Source builds select the first Cargo executable on PATH.
Before creating the isolated build environment, the Windows installer coalesces
inherited variable names that differ only in case, using the value Windows selects.
This normalization is local to the installer process; it does not change saved
user or system environment settings.
Installers emit stage messages to stderr before preparation and building; the native operation reports copying,
verification and PATH setup. Help emits no progress. Existing binaries skip build
messages, and `--no-path` reports that PATH setup is skipped. Cargo output remains
live; Windows also reports elapsed build time every 15 seconds while waiting,
without estimating a completion percentage.
Publication atomically refuses any existing destination, including a directory
created while the staged executable is being checked. A publication collision
uses the same concise diagnostic as preflight and removes the installer's staged
file without replacing the competing destination.
The Windows installer makes a newly created `bin` directory private to the current
user, SYSTEM and Administrators so it can support core upgrade. Existing directory
ACLs are preserved; shared installations require administrator-managed updates.
Installer failures emit concise stderr diagnostics without PowerShell source
locations or exception stacks. Parameter-binding tests use stable PowerShell
error IDs, independent of the user's display language.
PowerShell script output uses UTF-8 independently of the inherited console codepage.
Relative `CARGO_TARGET_DIR` values resolve against the source checkout, matching Cargo.
Linux and Windows CI build the native uninstaller before running the installer,
uninstaller and package behavior suites against isolated prefixes and homes.

`node apps/devtools/build.mjs core|web|desktop|ssh-helper ABSOLUTE_PACKAGE
[--debug] [--replace]` builds one independent package.
All native builds use the ordinary shared Cargo target directory. Web builds its
document, Worker and WASM; it does not build or replace the native core. Desktop
checks the loaded Web document bridge before opening its window. Web is installed
separately and may be updated independently. SSH helper builds its same-CPU Linux musl
target on Linux. Toolchain and system prerequisites are explicit; the producer
does not install compilers or libraries.
SSH publication also enforces the transport owner's 128 MiB image bound.
An unstripped debug helper exceeding that bound must be rebuilt in release mode
or supplied as an explicitly stripped artifact; it cannot become an unusable
installed helper merely because the generic package file bound is larger.
Build subprocesses receive declared tool inputs and a private home, without
provider credentials. `--replace` supports a stable source-recipe output: build
and verify a private candidate, replace its declared files, and publish the
manifest last. Existing nonempty output must name the same package and target;
an empty ordinary directory can recover a first publication interrupted before
its owner marker was written. Unrelated
files are preserved. The Host source-build manager serializes recipe writers;
manual producers require separate outputs. Interrupted publication
can leave an inadmissible candidate, while the installed immutable package stays
unchanged; the next build can replace that candidate without a persistent lock
file or a separate recovery command.

## Development supervision

Use `rsi plugin install --local PACKAGE` to install an optional package, then
`rsi web`, `rsi desktop` or `rsi run PROFILE`. Development uses the same binary,
homes and commands with `--watch`; trusted recipes belong to
[`rsi.toml`](../../crates/rsi/core/docs/launch.md). There is no separate development
runtime or source-directory launcher.

`node apps/devtools/packages.mjs assemble KIND OUTPUT INPUT [UNINSTALLER]`
assembles already built artifacts. Output must be new. Payloads are hashed and
copied in 64 KiB chunks, then verified before the manifest is written. The package
owner's disk bounds cover unstripped debug binaries without making them buffer
sizes. `RSI_RELEASE_VERSION` labels the package. Requirements come from the
artifact's actual wire contracts, not a native build family.
Core requires the uninstaller input and includes the platform uninstall launcher;
both native exports and the launcher are verified before publication.

`fixture-dist` binds the document bundle identity to its captured build-family
digest. This lets evidence fixtures verify the exact native/assets publication;
it does not make that digest a production wire-compatibility requirement.
Web selects those contract owners from the Worker package's explicit Host-interface
declaration and verifies that each is a WASM normal dependency. It does not infer
remote requirements from every codec in the transitive dependency graph.

`node apps/devtools/packages.mjs sign ROOT NAME SEQUENCE` validates the complete
repository before signing `repository.json`. Publisher-side manifest checks use
the package owner's acceptance rules and shared conformance cases, including
export membership, executable roles, dependency identity and interface bounds. It receives a PEM Ed25519 private
key in `RSI_REPOSITORY_SIGNING_KEY` and its pinned public key in
`RSI_PACKAGE_REPOSITORY_KEY`. Signing is separate from build jobs. Repository
metadata expires in seven days and must be refreshed with an increasing sequence.
Real publishing credentials are supplied by the operator, never by test fixtures.

## Fixture development

The Rust executable retains `fixture-dev`, `fixture-dist`, `fixture-path`,
`fixture-build`, `fixture-lease` and `gc` for isolated conformance fixtures that
exercise legacy bundle admission, addon loading and process cleanup. Fixture
publications are not product installation or launch entrypoints. Their leases,
atomic publication records and bounded child diagnostics remain owned here.
Fixture development selects its service through its private `rsi.toml` and
stops its resident Host before retiring the fixture homes.

Run Node producer and installer behavior tests with `node --test
apps/devtools/tests/*.test.mjs`, and Rust fixture tests with `cargo test --locked
-p rsi-app-tools`. Real source builds and platform integrations are opt-in.

All build entrypoints reject malformed environment declarations; Windows SDK
names containing parentheses are valid even when a POSIX producer has no value
for them. Private directory suffixes contain nonempty bounded path components.
The legacy fixture distributor retains its target-scoped private home; ordinary
package producers and minimal installers share `target/rsi-build`.

Build producers share `build-environment.txt` for forwarded tool inputs. Fixture
captures deliberately replace `CARGO_TARGET_DIR` with their isolated captured
target and use separate private homes; ordinary package builds use the caller's
selected Cargo target. These directory choices are not wire identities.

Installation and removal tests require a prebuilt native `rsi-uninstall` at
`target/debug` or `RSI_TEST_UNINSTALLER`. Build it with `cargo build -p rsi-uninstall`
before running the isolated Node suites; their fixtures never build during uninstall.
