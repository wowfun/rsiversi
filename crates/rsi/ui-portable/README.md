# rsi-ui-portable

`PortableUiFactory` imports one explicitly injected `rsi.ui.portable` capability
as ordinary Local UI contributions. It reads and validates Describe before
publishing any bundle. Asynchronous presentation materialization, invocation and
source reads use that exact generation-fenced capability; synchronous drawing
uses the owning PresentationLease's already captured snapshot.

The [protocol](../ui-protocol/README.md) carries opaque presentation identities,
models and bounded action data. Context, Local mapping keys and authenticated
origins never enter this wire. No domain authority is inferred from those opaque
identities. With explicit `business_api: true` configuration, each presentation
binds one ordinary child Profile beneath its real target. That Profile requires
the target's `UiBusinessApiContract`, injects the UI source capability, and exports
the already narrowed client through `rsi-api-portable`. Both capabilities then
have the same Meta holder. Snapshot, action and source requests carry the semantic
scope and exactly one transferred API grant. Describe carries neither. There is
no fallback to an ambient ApiClient or an invented Context reconstructed from JSON.
The child Profile remains alive through admitted actions and closes with the
presentation. Its Portable API grant cannot be used after that generation retires.
The adapter registers no Session policy or runtime.

Each request and JSON reply fits one 128 KiB message; a source reply is raw bytes
within its requested maximum of 64 KiB. One reply must be followed by clean
Portable completion. Missing, duplicate, capability-bearing, oversized or failed
terminal replies are errors. A shared 64 MiB wire pool reserves before encoding,
copying or retaining replies. Meta separately accounts queued Messages. These
are encoded-byte bounds, not process RSS limits.

Retirement withdraws the Local contribution and drains its admitted work through
the existing UI/Meta owners. Dropping an action waiter never cancels a dispatched
mutation. Protocol failures cancel and drain the call; no automatic action replay
is attempted. Source invalidation is owned by the contributing application;
this adapter does not invent a polling runtime or a source-version ledger.
