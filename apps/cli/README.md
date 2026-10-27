# rsi-cli

Top-level help groups Usage, Applications, Commands, Global options and Examples.
Each public entry has one summary; application owners maintain detailed help.
Nested management help is recognized before home maintenance or path-value
admission, even when a preceding option lacks its value. Arguments after `--`
remain literal. Unknown command paths remain errors with the owning command help. Custom Profile help explains
invocation without loading the Profile. Internal protocol probes stay hidden.

Recorded core installations use the
[installation owner's transaction](../../crates/rsi/installation/README.md);
legacy unrecorded executables retain the single-file transaction.

On Windows, persistent-Host integration tests must execute outside Cargo's child
Job. Compile with `cargo test --no-run --message-format=json`, then execute the
reported test artifacts from the native caller. CI does this for CLI and catalog
targets. A launcher whose Job forbids breakaway receives an explicit startup
error; the product never substitutes a transient embedded service.

For artifact acceptance, set `RSI_TEST_PRODUCT_BINARY` to the absolute installed
`rsi` path when running `service_host_cli`, `windows_acp`, `resident_listeners`,
`core_upgrade` or `addon_cli`. These fixtures retain their isolated homes and deterministic
providers while exercising those exact product bytes. Without the opt-in they
use Cargo's binary; paired-Web cases still require their own complete publication.
Setup fixtures select their Service explicitly; missing-route cases must name an
unavailable model in that Service rather than depend on a different launcher default.

The native RSI launcher owns command parsing, process control and Tokio setup. It
uses the explicit application catalog for native clients and daemon startup.
An immutable managed publication starts its resident Host from the original
executable and holds its generation lease through readiness; the child then
retains its own lease through shutdown. Ordinary build outputs are first copied
to the private content-addressed Host runtime so the resident process does not
lock Cargo output. Managed executables must never be copied away from their
publication marker and lease for daemon startup. After readiness, both startup
paths collect recognized obsolete private Host artifacts while retaining the
startup coordinator. Managed generation pinning is independent of that private
namespace; an absent private artifact directory needs no cleanup.
Windows batch trampolines use the same managed executable and pin its generation
before dispatching the batch. Private artifact collection retains executable
directories still in use by Windows processes. Runtime publication resolves the
source path before detecting its managed generation on every platform.
`cargo run --locked -p rsi-cli -- --help` does not activate a backend.
The native executable always includes the linked application catalog. Web and
Desktop are installed independently with `rsi plugin install`; application
selection uses `rsi web`, `rsi desktop` or `rsi run NAME`.

For a manual native build, run from the repository root:

```bash
cargo build --locked -p rsi-cli
target/debug/rsi --help
```

The native executable includes all of its linked backends. Application Profile
selection changes runtime composition, not the executable's Cargo dependency
graph. The [development guide](../../docs/development.md) starts the standard
debug product with caller-selected state; [source installation](../../docs/user/source-install.md) owns
minimal installation and independent optional package builds.

Product Web integration tests are opt-in: build a complete paired publication,
then run `RSI_PAIRED_BUNDLE="$(cargo run --quiet --locked -p rsi-app-tools -- fixture-path web)" cargo test --locked -p
rsi-cli --features paired-web-tests --test service_host_cli web::`. The directory
must contain its paired `rsi`, `assets/` and receipt. Missing inputs fail explicitly;
these tests never fall back to an ordinary Cargo executable and arbitrary assets.

Headless process tests synchronize the phase they exercise before sending a
signal. Windows ACP integration exercises the built native binary with isolated
XDG paths and a loopback model fixture, including model/effort selection, cold
resume and stdio EOF. Linux MCP fixtures remain Linux-only. Tests of unavailable
capabilities continue to assert refusal before effects.
Provider entry establishes execution readiness; the cancellation-output
test also waits for the complete JSONL Turn record. These bounded waits retain
captured output on failure and reap the child. The
[terminal contract](../terminal/README.md) owns interruption and output deadlines.

Debug and release use the same launch parser, final home selection and resident
Host path. `--watch` adds connection-owned configuration/source leases; Vite and
Desktop startup use bounded private pipes owned by ordinary Application plugins.
There is no debug-only launch grammar or source-tree UI selector.
The [launch maintenance contract](../../crates/rsi/core/docs/launch.md) owns
global config/state resets, explicit import and recoverable core replacement.
Application help is resolved before home initialization or maintenance.

`rsi agent-store verify` audits an existing, cleanly closed Store without creating it.
`rsi agent-store compact` obtains the exclusive Store writer lease, refuses pending
Session/CAS retirements, and performs SQLite VACUUM. Stop the Host first. It changes
the database file layout, preserves logical records, and reports database bytes
before and after; it does not collect CAS. Both commands accept an absolute `--root`
and `--output text|json`. JSON uses version 1, a command-specific `type`, `status`
and `root`; compact additionally reports `before_bytes` and `after_bytes`.

The `core_upgrade` integration target also requires a prebuilt `rsi-uninstall`
(next to the test product binary or selected with `RSI_TEST_UNINSTALLER`). Windows
Host tests run their compiled test executable outside the Cargo Job.

Core upgrade and recovery emit one JSON receipt on stdout; Host progress goes to
stderr. `host stop` reports stale metadata without claiming it stopped a dead PID,
and graceful failure also names its explicit `--force` escape hatch.

The managed core-upgrade fixture uses Node to assemble a real devtools package
before admitting it through the CLI; build `rsi-uninstall` first.
