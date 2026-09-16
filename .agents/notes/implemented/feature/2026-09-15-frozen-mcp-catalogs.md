---
name: Frozen external tool catalogs and managed protocol transports
---

## Problem

Tools must be declared before composition seals their catalog; cold resume
needs complete saved definitions without contacting an external server. Finite
batch stdin and lossy stdout tails cannot carry an ongoing JSON-RPC protocol.

## Decision

Composition accepts a bounded opaque Domain seed before activation and includes
it in generation identity. Fresh source snapshots carry current verified manifests;
restoration passes the immutable saved baseline. The integration's own codec
validates its manifest and declares the complete frozen tools before sealing.
Kernel does not learn MCP. One MCP Domain stores at most 256 KiB without truncation
or sharding; the existing 1 MiB baseline and 64-tool ceilings also apply.

MCP owns HTTP/JSON-RPC, epochs, credential resolution, explicit enabled endpoints
and selected tools. It preserves verified manifests across temporary disconnects,
invalidates verification after reconnect or list changes, and returns typed tool
errors for expected unavailability. Instructions are external data and existing
ToolPolicy remains the authorization owner. New catalogs affect new generations.
A separate Process duplex contract shares existing process/capture admission,
provides persistent stdin, lossless backpressured stdout and bounded stderr,
and retains cancellation/reaping ownership. Only Local configuration can select
absolute stdio commands, argv and cwd; started calls are never automatically replayed.
Local stdio configuration authorizes a Host service outside any Session sandbox;
its Sandbox plan explicitly requests unconfined execution. This is not a
confinement guarantee for untrusted server code. The
[MCP transport contract](../../../../crates/rsi-mcp/README.md) requires TLS for
bearer endpoints outside explicitly pinned loopback. Applying Retrieval's public
DNS policy here would prevent intentionally configured private MCP services.

A fresh-draft continuation lease pins the original composition before a fresh
Session's first mailbox claim. Kernel cold selection reuses that exact pin until
the lease is released. Durable-session leases still fail across a changed cold
generation. Rebuilding from the saved seed at this point would replace
the authority that admitted the automatic input.

## Alternatives considered

Mutating sealed Tools would change an existing Session's meaning. Decoding MCP in
Kernel would reverse ownership. A hash without the complete typed manifest cannot
reconstruct definitions offline. Lossy tail readers cannot distinguish an intact
JSON-RPC frame after overflow. Direct subprocess creation would bypass shared
Process admission and retirement ownership.

## Consequences

Deterministic HTTP and stdio servers prove complete discovery, selection, paging,
call errors, cancellation, credentials, reconnect/list invalidation, offline
restore and new-generation catalog replacement. Manifest/Domain/tool ceilings fail
without partial exposure. Duplex backpressure retains bytes while active and within the bounded final drain;
drain expiry reports an error. It shares process/capture limits with batch jobs
and drains/reaps on cancellation and retirement. Visual
status/actions and opt-in live integration evidence are recorded independently.

### Trade-offs

Manifest updates, transport epochs and composition generations must remain distinct.
Restore must not silently contact a server or replace a saved definition. Caller
cancellation cannot recycle permits before transport/child work ends. Backpressure
must stop production without holding a lifecycle mutex; retirement must still be
able to close pipes and reap the child. External schemas and response frames need
bounds before materialization, not just after JSON decoding.


### Current protocol and interoperability

The client implements the stateless 2026-07-28 core alongside the existing legacy
transports, following the official [versioning source](https://github.com/modelcontextprotocol/modelcontextprotocol/blob/cd0623765886c8cc282e3e5e1a03ab7469055fab/docs/specification/2026-07-28/basic/versioning.mdx)
and [HTTP binding](https://github.com/modelcontextprotocol/modelcontextprotocol/blob/cd0623765886c8cc282e3e5e1a03ab7469055fab/docs/specification/2026-07-28/basic/transports/streamable-http.mdx).
Changing a version string would leave the removed handshake, session IDs and GET
stream in use. The transport therefore owns explicit era detection, per-request
metadata, HTTP header projection and correlated subscription acknowledgment.
Only a silent discovery probe may restart stdio once in legacy mode, after the
first child has been reaped. Business calls remain single-attempt operations.

An MRTR result is unfinished work. The integration advertises no input-producing
client capabilities and surfaces additional-input requirements without synthesizing
answers or replaying the call. Extending this with elicitation, Apps or Tasks would
require a separate owner and human interaction contract; enabling them implicitly
would expand the integration's authority. Manual bearer credentials remain scoped
to configured endpoints; this change does not introduce OAuth discovery or login.

Invalid HTTP parameter annotations reject the complete catalog. The specification
recommends omitting just the malformed Tool, but that would violate the existing
complete discovery and selected-Tool contract without an explicit rejected-Tool
projection. Header values preserve UTF-8 and exact safe integers without rounding.
Cache TTL/scope metadata is validated; it never erases historical definitions or
shares cached response bodies across credential contexts.
