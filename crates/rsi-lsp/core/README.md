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
four live processes with one nonqueued query per workspace. Up to four admitted
source validations may reserve empty workspace slots; invalid input cannot evict
a healthy process. A dropped caller cancels subsequent
work but keeps its admitted task, permit and process until cleanup finishes.
Locally rejected source paths and coordinates preserve an existing healthy process;
only failure after connection work begins retires that workspace's connection. One
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
Initialization advertises only static read-only capabilities. Bounded configuration
and progress-create requests are answered; applyEdit and other methods are rejected.
Configuration has at most 32 environment entries / 32 KiB, 32 arguments / 16 KiB,
32 language mappings and 16 KiB initialization/configuration JSON.

Normalized results contain at most 128 locations, 16 KiB total relative path bytes,
or 16 KiB hover text. Only contained local file URIs are openable. Foreign schemes,
outside-workspace targets, invalid ranges and oversized responses fail explicitly.
Result contract `rsi.lsp.query`, version 1, drives the model projection and shared
read-only UI. The Tool derives workspace and sandbox authority from its actual
native Agent caller; arguments cannot replace them.

The product [language UI adapter](../../rsi/lsp-ui/README.md) owns presentation.

Deterministic fake-server tests are keyless. Real acceptance uses the repository's
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
