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
current pure Context projection, recorded options and recorded language profile,
is always approximate and
identifies its implementation and limitations. Missing evidence or reconstruction
capacity produces an explicit unavailable diagnostic, never fabricated input.
Shared-pool pressure is transient: the same accepted watermark can yield an
unavailable request diagnostic under pressure and an approximate reconstruction
after capacity is released. Persisted sections retain the same watermark and
content; the diagnostic does not replace historical evidence.
Missing historical language profiles likewise make reconstruction unavailable;
export never consults a current provider or invents historical image capability.
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
Before final persistence, explicit cancellation removes the temporary file and
returns `FileWriteError::Cancelled`. The last token check after file synchronization
is commit admission. Once admitted, persistence runs to its actual success or
filesystem failure; late cancellation does not change that result. Dropping an
admitted wait future loses confirmation and cannot promise that no file was saved.
Application owners retain their worker through completion. Stdout cannot retract
bytes already delivered and reports failure separately.

Tests exercise this public stream and sink with isolated deterministic Stores,
including selection, inherited history, request evidence, concurrent append,
large artifacts, backpressure, malformed streams and interrupted file output.

Native facts preserve Model and Program Tool origins and the sealed program role.
Program evidence remains exportable even though its internal Tool results are
excluded from provider Context. Human-readable projections label that origin
without fabricating a model request for an internal program call.

Diagnostic request reconstruction receives the service's shared Context budget.
It retains admission through speculative reconstruction and serialization rather
than opening an independent pool alongside executing or parked Sessions.
Pressure at any reconstruction stage reports `context_capacity`; semantic failure
reports `semantic_reconstruction_unavailable`. Neither is a historical request.

Evidence resolution uses conservative workspace for recorded text, decoded sections
and metadata; reconstructed requests separately cover overlapping message/JSON
copies. Pretty-render admission measures the selected output format: JSON retains
one output string, while Markdown covers the pretty string, dynamically sized
backtick fence and complete fenced output while they coexist. The producer retains
these credits through the yielded document string. Pressure makes the diagnostic
unavailable without model I/O. Evidence/request workspace factors are policy
headroom, not proven allocator bounds or measurements of RSS.
