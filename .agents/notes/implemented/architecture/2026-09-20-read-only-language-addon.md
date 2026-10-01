---
name: Independently composable read-only language queries
---

## Problem

Text search cannot resolve definitions or implementations. A language server adds
process, document synchronization and request lifetime authority that does not
belong in the Agent Kernel. Returning arbitrary URIs must not create file or edit
capabilities.

## Decision

An optional ordinary source addon uses public Process/Sandbox/Files,
with four closed semantic queries, a typed Tool output and a service UI shared by
all product clients. It bounds and serializes each workspace connection, retires
it on cancellation or protocol uncertainty, and rereads files at their owning boundary.
The [plugin contract](../../../../crates/rsi-lsp/core/README.md) owns exact limits.

UI pages share one retained normalized result. Rerunning the query for each page
can skip or repeat locations when current files change, so only Repeat query
performs fresh I/O. Bounded result retention trades expiring old cursors for
stable pages; each cursor remains bound to its actual Session, workspace and
provider generation.

## Alternatives considered

A generic JSON-RPC Tool would expose edit and command authority. Automatic server
installation adds package and credential policy unrelated to query semantics.
Embedding LSP inside Kernel or adopting an editor session would add ownership that
this feature does not need. A server per query avoids pooling but repeatedly pays
index initialization; a bounded per-workspace pool preserves reuse and retirement.

## Verification

An independent source addon uses only public SDKs and exercises provider generation
replacement and cleanup. Fake peers prove Unicode positions, current-file updates,
framing limits, cancellation, hangs and exit. The pinned rust-analyzer completes
all four actual queries. TUI, Chromium/Firefox and Linux desktop open the returned
file and position with no model or external editor needed for inspection.

## Consequences

Language servers may execute their own workspace helpers; read-only confinement
and explicit operator selection bound that authority. Language results can be
stale relative to later file changes. Unsupported encodings, outside-workspace
locations and oversized results fail explicitly. Cancellation retires the pooled
connection, trading reinitialization cost for an unambiguous request lifetime.

The provider and Tool remain independent of standard-product crates. The
[product UI adapter](../../../../crates/rsi/lsp-ui/README.md) owns Session-controller
and Service-UI integration; its target-scoped requirements are resolved on the
selected native conversation, not at provider-root activation.

Execution location is part of source authority. Each query retains one complete
ExecutionLease for Files, program resolution, Sandbox and Duplex, and the pool
includes that exact lease generation. DSH `packages/lsp/lsp-stdio/src/host.ts`
reads through the filesystem capability paired with subprocess execution;
`index.ts` keys the pool by the host filesystem target identity. RSI additionally
needs the original revocable delegation and provider epoch in that key: path or
location equality cannot allow a new caller to borrow a previous process's grant.
Remote executable/environment configuration is separate from Local configuration,
so target account defaults never come from the Service's environment snapshot.
The product UI publishes its source capability only after binding the actual API
origin, before controller activation; each finite worker retains that authority.
