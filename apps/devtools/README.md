# rsi-app-tools

The source installers in `scripts` install only the native `rsi` executable and
configure the user's PATH. They do not select applications, change homes, install
plugins or run the product. An existing installation is upgraded through the
core transaction, rather than overwritten by the bootstrap installer.
`--binary` / `-Binary` installs an explicitly trusted existing executable;
`--prefix` / `-Prefix` chooses the installation root, and `--no-path` / `-NoPath`
supports isolated installation verification.
Publication atomically refuses any existing destination, including a directory
created while the staged executable is being checked.
The Windows installer makes a newly created `bin` directory private to the current
user, SYSTEM and Administrators so it can support core upgrade. Existing directory
ACLs are preserved; shared installations require administrator-managed updates.
Installer admission tests use stable PowerShell error IDs, independent of the
user's display language.

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

`node apps/devtools/packages.mjs assemble KIND OUTPUT INPUT`
assembles already built artifacts. Output must be new. Payloads are hashed and
copied in 64 KiB chunks, then verified before the manifest is written. The package
owner's disk bounds cover unstripped debug binaries without making them buffer
sizes. `RSI_RELEASE_VERSION` labels the package. Requirements come from the
artifact's actual wire contracts, not a native build family.
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
