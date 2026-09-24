# rsi-session-export

Session export reads one immutable Header, its accepted durable Fact watermark,
and its exact direct-parent inherited interval. It never resumes execution,
contacts a provider, refreshes tools or reads current workspace instructions.
An unpublished draft exports its frozen Header and empty history without publishing it.
Every section selection, including last-request/response diagnostics, is valid for
a draft; absent provider calls produce explicit unavailable diagnostics.
The Session owner authorizes access and admits at most two simultaneous exports; this library consumes the mechanical Store.

Markdown and JSON select `header`, `messages`, `reasoning`,
`provider-input-evidence`, `last-provider-request`, and `last-provider-response`.
Short include names are `h`, `m`, `r`, `pie`, and `lpr`. The default is messages;
reasoning implies messages. Unselected sections are absent. Header metadata
excludes instruction text. Messages preserve persisted conversation and Tool
content, optional readable reasoning, media references and partial output; they
exclude injected instructions and opaque provider reasoning state. Each record
retains its source Session and Fact coordinates. Internal compaction output is
not an Assistant answer. Child Sessions are exported separately.

Request and response diagnostics select the same latest completed Conversation
effect within the accepted watermark. Request evidence resolves exact section
references and verifies their digests. Semantic message reconstruction uses the
current pure Context projection and recorded options, is always approximate and
identifies its implementation and limitations. Missing evidence or reconstruction
capacity produces an explicit unavailable diagnostic, never fabricated input.
Response output is reconstructed from normalized durable events, not raw HTTP.
Neither diagnostic contains credentials or claims exact provider wire replay.

Export retains bounded Store pages and emits at most 64 KiB UTF-8 per chunk,
under consumer backpressure. Start fixes source identity and options; completion
reports total bytes and SHA-256. Missing completion, wrong offsets, changed
identity, oversized chunks and digest mismatch are failures. The stream has no
whole-artifact byte limit. Cancellation yields one error then EOF, releasing the
producer, pending reads and validation leases; it never emits a successful completion.
The native sink writes a unique temporary file beside the destination, creates
parents as needed and replaces the destination only after verified completion.
Failure and cancellation remove the temporary file. Stdout cannot retract bytes
already delivered and reports failure separately.

Tests exercise this public stream and sink with isolated deterministic Stores,
including selection, inherited history, request evidence, concurrent append,
large artifacts, backpressure, malformed streams and interrupted file output.
