---
name: Plugin-composed Web and Linux desktop GUI
---

## Problem

The existing Web application already owned bounded Session controllers,
recoverable submissions and renderer lifetimes. A desktop product needed the
same behavior, first-run configuration and workspace navigation. Duplicating
Session or provider policy inside a platform shell would create competing owners.

## Decision

The shared [GUI application](../../../../crates/rsi/gui/README.md) owns Session
surfaces and presentation. The [Worker](../../../../crates/rsi/web/README.md) and
[Linux desktop](../../../../crates/rsi/desktop/README.md) are transport/platform
adapters. Tauri owns the main-thread event loop and private persistent WebView
storage. Its types do not enter shared business packages or the headless binary.
An awaitable [Application lifetime](../../../../crates/rsi/application/README.md)
retains actual Runtime cleanup before platform exit. Failed draft persistence
cancels the close attempt and its deadline, preserving recoverable editor input.

[Application composition](../../../../crates/rsi/core/README.md) carries ordinary
Application-only extras separately from the Service addon digest. A desktop
renderer must not change the desired Service merely by being linked. Native
catalog refresh and factory reservations retain those extras without leaking
Application declarations into Service compatibility.

A [paired distribution](../../../../crates/tools/rsi-xtask/README.md) builds desktop
and headless companion from one frozen capture of current tracked and non-ignored
source bytes, including dirty files. Recorded source bytes, modes and symlinks
are validated by the Cargo build script before embedding and registered as Cargo
rebuild inputs. Packaging also rejects added source outside generated dependency
and build directories. Both embed one build-family digest; individual
executable hashes remain artifact identity. The canonical companion is also the
coding helper. The existing exact launch key and protocol epoch still apply;
family identity grants no authentication. A borrowed daemon survives GUI teardown.

First-run configuration uses the existing three provider factories through an
ordinary [managed-provider owner](../../../../crates/rsi/managed-providers/README.md).
Source Profiles remain source-owned. Missing Agent defaults are a valid unconfigured
state, while Session creation still requires strict frozen settings. Provider
application, credential changes and default selection have independent receipts
because their persistence owners cannot commit one atomic transaction. Unknown
outcomes are displayed without replay. A source conflict retains desired state
and a convergence diagnostic instead of pretending the new route is active.

The [configuration authority](../../../../crates/rsi/configuration-access/README.md)
is durable and Local-owned. It gates old Settings writes as well as new provider
APIs. A Device grant is powerful configuration authority, not permission to author
arbitrary Profiles or mutate every plugin namespace. Revocation fences admission
and drains prior mutations. Credential status exposes effective source/editability
without transporting secret resolution to a Device.

The [navigation owner](../../../../crates/rsi/navigation/README.md) stores title and
archive metadata independently of Session history. Query continuation fences its
filter, metadata revision and Host generation, and can advance through empty pages.
Workspace grouping uses exact registered paths. Archive changes visibility without
detaching a resident Session. Empty-draft reuse requires the current owned, matching,
unpublished attachment, no pending submission and unchanged captured defaults.

The [document](../../../../plugins/rsi/web/README.md) uses one pinned React runtime
and selected DSH SlotCore/binding/primitives with exact
[vendoring provenance](../../../../plugins/rsi/web/vendor/dsh/provenance.json).
Workspace/Session navigation, selected Chat/Trajectory, Session resources and global
Settings are separate feature surfaces. Existing UI contracts retain action
identity and independently contributed views. The former Session DOM renderer
remains an explicitly initialized island with its own awaited mount lifetime.

Vite refreshes feature components in development; bootstrap changes reload the
document. Independently published renderer graphs use candidate admission,
mount/disposal and exact ACK instead of framework HMR. This preserves resident
Session state while executable renderer assets change. Keyed surfaces keep bounded
persistent editors; the schema upgrade preserves uncertain submissions from both
old numeric panes transactionally without increasing the origin allowance.

Immutable block revisions let a presentation baseline retain closed JSON values
and encoded lengths for unchanged blocks. Changed-pane metadata remains cheap
and independently projected; snapshot and patch serialization borrow the block
cache. This avoids interpreting a small wire patch as proof of small producer
work. The cache belongs to one bounded baseline and carries no durable replay
or business authority; generation changes discard its reuse proof.

## Alternatives considered

Keeping only the vanilla document would retain its small build surface but would
not provide the selected DSH composition interfaces. Rewriting the whole Session
renderer in React would replace proven DOM identity and persistence behavior
without being necessary for composition; retaining the island is a deliberate
integration choice, not an unavoidable platform limitation.

Electron would better match DSH's complete desktop/plugin execution environment.
This milestone instead keeps Rust as the runtime owner and imports only the
selected DOM composition closure. It does not claim DSH Host/plugin compatibility,
dynamic desktop library replacement, or a shipped DSH TUI.

## Consequences

Tauri's owned response handoff requires a copy of shared retained bytes, and Wry
buffers requests before the owner callback. Logical request/frame bounds therefore
do not imply an RSS bound or zero-copy WebKit delivery. One pending frame, exact
ACK and reserved lifecycle admission bound the application-owned queue. Whole
WebKit process-family memory is measured separately.

IndexedDB is the document draft owner; native Settings/Storage and Credentials
retain their existing owners. Keeping their independent outcomes visible avoids
an unsupported claim of cross-backend atomicity. Neither a reload nor a lost reply
is evidence that replaying a mutation is safe.

## Validation boundary

The [Web product fixtures](../../../../fixtures/rsi/web-product/README.md) exercise
Chromium/Firefox, persistent conflicts, unknown outcomes, renderer replacement,
actual source actions and explicit live-provider opt-in. The
[desktop fixtures](../../../../fixtures/rsi/desktop-product/README.md) use actual
Linux Tauri/WebKitGTK, Chinese input, raw protocol admission, ACK deadline failure,
failed-save recovery, window close/restart and paired daemon attachment.

The performance contract separates source-preserved uncached projection CPU from
real-engine document input/paint and process-family PSS. Synthetic scenes prove
neither live-provider latency nor physical display scanout. Visual inspection and
body/input assertions exclude malformed or blank scenes. Native Windows/macOS
remain outside this Linux delivery; Linux/WSL evidence does not establish them.
