---
name: Independent product packages and protocol admission
---

## Problem

Whole-publication build families prevent optional Web and Desktop packages from
working with an independently built core. Existing file hashes do not authenticate
downloaded executable content.

## Decision

One minimally usable core supplies the command and linked foundation plugins.
Optional packages publish bounded manifests, immutable file objects and exact
dependency closures. Profiles select activation; a state-owned resolution lock
selects installed bytes, without another enabled map.

Owning protocols export generated wire descriptions and explicitly versioned
semantic contracts. Consumers negotiate their required interfaces before use.
Web document, Worker and WASM remain internally paired. Signed official release
metadata authenticates target manifests and protects normal resolution from
expired metadata, rollback and same-sequence equivocation. Identical payload retries are allowed. Explicit local sources retain distinct provenance.

Desktop is a Tauri carrier for the Web document and Worker with narrowly scoped
native bridges. Its listener lease belongs to its connection, not to the browser
listener or the whole Service. Core upgrade explicitly stops the old Host.

This decision partially supersedes
[paired publication](../../implemented/architecture/2026-09-26-application-ownership-and-paired-web.md),
[build targets](../../implemented/process/2026-10-18-application-build-targets.md)
and [local Web launch](../../implemented/architecture/2026-09-26-local-web-launch.md).

## Alternatives considered

Disabling pairing removes integrity without replacing compatibility. A normalized
whole-repository family still couples unrelated interfaces. A package marketplace
or general version solver adds no necessary capability to exact official and
explicit local package resolution.

## Consequences

The [package contract](../../../../crates/rsi/packages/README.md) owns admission,
leases and signature trust. Independent producers and the release workflow use
the same manifest publication path. The minimal installer makes the core command
available; later optional installation leaves its bytes unchanged. Shared Cargo
caches replace per-UI product captures. Explicit fixture captures still serve
isolated testing and do not select ordinary product runtime.

The rejected normalized global family would still couple unrelated consumers.
Interface-specific generated contracts instead retain byte integrity inside each
Web bundle and compatibility at each external boundary. This deliberately
replaces native/Web whole-publication pairing, rather than weakening its checks.
Semantic validation still needs owning tests and reviewed version changes.

Core upgrade has one installation writer, verifies candidate bytes before stopping
the current Host, retains the previous executable and records recoverable rename
steps. The ordinary command refuses an incomplete transaction until explicit
recovery. There is no background updater. A successful package installation and
activation of that executable are distinct operations.
Once recovery is settled, receipts retain the transaction evidence while obsolete
executable copies are disposable. A Windows image still held by an executing
caller is retried at the next upgrade; cleanup must not invalidate a successful
activation or discard participants belonging to a pending recovery.

The Desktop carrier depends on the Web package and bounded native bridges. It
does not load product composition or locate a companion core. The SSH producer
also enforces the transport owner's 128 MiB immutable-image limit; a very large
unstripped debug helper requires a release or explicitly stripped build.

Official publication requires a real configured signing identity and endpoint.
Isolated signing fixtures establish tamper, replay and expiry behavior without
claiming an official release. System WebView2 and other platform runtimes remain
external prerequisites. Native macOS validation is a separate platform scope.

Signature verification produces an opaque repository authority. The package
owner persists its high-water receipt independently of payload success and admits
new signed manifests only against that exact authenticated index. Core network
replacement additionally requires the binary's compiled identity; ordinary plugin
configuration does not override that executable-replacement trust boundary.

Settled executable copies are deleted by validated receipt identity and ordinary
file kind, without rehashing bytes that will never be activated or restored.
Pending participants and newly acquired packages still require content checks:
a content-addressed pathname is not a proof against later filesystem mutation.
