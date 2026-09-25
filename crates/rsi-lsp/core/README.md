# rsi-lsp

The optional linked addon provides definition, references (including declaration),
implementation and hover. Operators explicitly configure an absolute executable,
arguments, extension-to-language mappings, environment and initialization options.
No server is installed automatically. Configuration freezes with its ordinary
Meta provider generation; retiring it cancels queries and joins server reaping.

The semantic query takes a canonical workspace, a relative file and one-based
line / Unicode-scalar column. Files validates no-follow workspace access and reads
at most 1 MiB of current UTF-8 source before starting a server. The plugin converts
columns to zero-based UTF-16 and rejects a non-UTF-16 server negotiation. A query
opens the current full document, performs one supported operation, then closes it.
Transient synchronization avoids stale document caches between queries.

Servers are pooled by canonical workspace within one provider generation, at most
four managed processes, including retirement, with one nonqueued query per workspace.
A replacement keeps the retiring workspace registered until settlement; new
workspace admission returns Capacity while any replacement is retiring. Up to four admitted
source validations may reserve empty workspace slots; invalid input cannot evict
a healthy process. A dropped caller cancels subsequent
work but keeps its admitted task, permit and process until cleanup finishes.
Locally rejected source paths and coordinates preserve an existing healthy process;
a previously failed idle connection is joined even when the next source query is
rejected locally. Healthy connections are retired only after connection work fails. One
query, including initialize and synchronization, has a 30-second deadline. Cancel
sends a bounded best-effort cancel notification and retires that connection; no
partially sent request or poisoned connection is replayed. Shutdown joins admitted
queries before releasing the pool. Processes use explicit environment and read-only
Sandbox confinement; the protocol never applies edits or executes server commands.

The Content-Length decoder admits at most 8 KiB of ASCII headers, 1 MiB per JSON
message, 4 MiB total per query, at most 256 server requests requiring replies, and
32 KiB retained stderr. Notifications share byte and time bounds rather than the
request count. JSON has unique object keys, depth at most 32 and at most 65,536
values. The decoder rejects conflicting lengths, malformed envelopes and mismatched
response IDs.
Each connection owns a continuously running protocol pump, including while idle.
One persistent whole-frame write advances alongside reads; incoming messages never
restart a partially accepted write. At most 256 compact server-reply descriptors
are outstanding, including the reply being written, with a thirty-second deadline
from the oldest admission. Overflow retires the connection. Processing yields after
32 envelopes. A query charges all unprocessed pre-read bytes on admission and every
subsequent stdout chunk until didClose, without resetting its absolute deadline.
Between queries, idle input has a cumulative 4 MiB / 256 server-request budget,
reset at didClose. Idle overflow retires the connection; silence has no TTL.
Idle failures are reaped immediately; only a subsequent new query can reconnect.
Retirement skips protocol grace after an uncertain partial frame. Otherwise it
discards obsolete queued server replies before attempting bounded cancel/shutdown
grace; an expired reply deadline cannot suppress that attempt. Grace is best-effort,
and retirement always joins actual Process settlement. A failed
protocol pump task is also terminated and joined by its connection owner; a task
panic never substitutes for Process settlement. A failed
settlement remains owned by its pool slot; when its join reports failure, the
connection enters a bounded retirement set and withdraws new admission. Dropping
the provider-close waiter leaves unvisited slots and admitted retirements owned. Provider close retries those joins and reports failure while any remain.
Query error categories are preserved independently of cleanup failures. The caller
deadline covers replacement cleanup, whose retained owner continues joining after
the caller returns. Cancellation is rechecked before eviction, confinement and spawn.
Initialization advertises only static read-only capabilities. Bounded configuration
and progress-create requests are answered; applyEdit and other methods are rejected.
Configuration items must be objects with optional string section/scopeUri; progress
creation requires a string or integer token.
Configuration has at most 32 environment entries / 32 KiB, 32 arguments / 16 KiB,
32 language mappings and 16 KiB initialization/configuration JSON.

Normalized results contain at most 128 locations, 16 KiB total relative path bytes,
or 16 KiB hover text. Only contained local file URIs are openable. Foreign schemes,
outside-workspace targets, invalid ranges and oversized responses fail explicitly.
Result contract `rsi.lsp.query`, version 1, drives the model projection and shared
read-only UI. The Tool derives workspace and sandbox authority from its actual
native Agent caller; arguments cannot replace them.

The product [language UI adapter](../../rsi/lsp-ui/README.md) owns presentation.

Deterministic fake-server tests are keyless. Their independent provider fixtures
share the process-wide Files job limit and bound fixture concurrency to that limit;
protocol-pressure tests still exercise real concurrent pipe progress. Real acceptance uses the repository's
pinned Rust 1.97.0 rust-analyzer component, with a private Cargo project and build
scripts / procedural macros / check-on-save disabled. It is a development fixture,
not a new semantic dependency of repository code-check. An independent addon
workspace exercises the public source-composition/testkit path and all four queries.

A newly initialized server can report no locations while indexing. Empty results
remain visible; Repeat query explicitly reruns the displayed read against current
files without starting a model turn. The client does not infer index completion.

Well-formed server errors retain only their signed JSON-RPC code, never the
server message or data. They retire the connection and remain visible with an
explicit Repeat query action; transient server refusals are not auto-replayed.

A query always attempts didClose after a response failure. If both fail, the
original query category remains authoritative; a close failure after a successful
query is still an error and retires that connection.

An idle protocol-pump panic publishes connection failure, closes request admission,
and terminates and joins the Process without waiting for another query or close.
