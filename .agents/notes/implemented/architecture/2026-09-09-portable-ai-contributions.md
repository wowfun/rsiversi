---
name: Native AI providers use existing prepared calls and registrar gates
---

## Problem

Language and Image callers consume Local adapters, while native ABI plugins
publish Portable services. Native integration must preserve the provider's
compatibility preflight, one-shot effect boundary, credential and media owners,
and route withdrawal. AI requests also exceed Meta's single-Message limit.

## Decision

The ordinary [Portable provider](../../../../crates/rsi-ai/portable/README.md)
imports an explicit business capability and publishes enabled facets through
ProviderPublication. A bounded Describe result supplies exact model support for
synchronous preflight before effect dependencies. Prepare exchanges only the
frozen request and redacted snapshot and returns bounded transient state. Start
consumes the existing Prepared closure with that exact request, state, context
and Portable generation. No new native registry or prepared-token table exists.

The [protocol](../../../../crates/rsi-ai/protocol/README.md#portable-provider-transport)
fragments JSON and binary packets under caller-owned byte admission. Media and
credentials cross only in Start-time binary replies. Request descriptor membership
precedes the original resolver's admission and verification. Native diagnostic
text is replaced with static errors; the bridge does not infer non-dispatch from
malformed native output. Semantic terminal delivery waits for clean Portable EOF.

## Alternatives considered

A native-only router would duplicate provider admission and withdrawal. Blocking
native RPC inside synchronous compatibility checks would hide I/O and lifecycle
failure before credential admission. Whole-request Messages would reject valid
existing request sizes or require changing Meta limits. JSON encoding binary
media would add expansion and risk crossing durable/Debug boundaries. Retaining
native opaque handles in a map would require another bounded lifetime protocol;
transient prepared state carried by the existing consuming closure avoids it.

## Consequences

The standard factory catalog exposes the bridge without enabling it by default;
global route updates require drain/restart. Native code has process authority,
and this business protocol cannot attest its behavior or sandbox it. Call
cancellation drains the Meta driver; failed native cleanup can still retain a
mapping under the Loader contract. Even cooperative cancellation can finish the
Meta driver before foreign callback exit; the next call may meet the existing
fail-fast busy/reentrant gate. Tests distinguish that terminal from eventual
reuse with pure Prepare probes and final Loader accounting. No external effect
is retried by the adapter.

Wire retention, queued Meta Messages and decoded semantic values have distinct
owners. Their bounds are not an RSS guarantee. A typed JSON round trip rejects
ignored nested fields without altering existing Language enum shapes. This
requires producers to preserve the typed serialization shape, including its
explicit option/default fields. The native fixture and public-router fault tests
exercise these paths without a live provider; production DeepSeek protocol and
live evidence remain independently owned.
