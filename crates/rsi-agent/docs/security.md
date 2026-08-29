# rsi-agent security

Durable and externally decoded values are validated by their owning protocol
before entering trusted runtime state. Session identifiers, canonical paths,
model routes, JSON values, Facts, Tool results, Media references, batch sizes,
and CAS bytes all have explicit finite bounds. Custom deserialization must not
bypass constructor invariants.

The session header records redacted configuration facts only. It may contain a
credential reference but never a resolved secret. Provider error summaries and
Tool failures are bounded before persistence. Media content remains owned by
the Media service; Agent Facts retain immutable references only.

SQLite owns files below its configured root and acquires an exclusive writer
lease before schema or recovery access. It rejects symlinked or non-directory
roots that cannot be canonicalized safely. CAS publication writes new immutable
objects and verifies their digest; cleanup must never follow or delete a path
supplied by a session Fact.

The Store's derived turn rows are committed in the same transaction as their
canonical Facts. Reopen checks their relational and lifecycle consistency;
Kernel recovery never trusts an index row without decoding and validating that
turn's bounded Fact stream.

The Kernel accepts an external-effect start marker only after its matching
intent is durable; the executor then durably flushes that start before
invocation. Recovery preserves a durable cancellation as `Cancelled`, treats
other unfinished work as interrupted, and never guesses that replay is safe.
Cancellation is cooperative, so every provider and Tool call also remains
bounded by its own timeout and shutdown deadline.

Turn acceptance stores an already-resolved execution policy rather than a
security-looking override. Danger-full-access is invalid without live approval;
restricted Tool process plans cross the pinned Sandbox service and durable
Tool results retain its actual enforcement stamps.

SQLite readers project each row's byte length and suppress an oversized header
or Fact body before allocating its Rust String. Typed Fact readers then enforce
both item and aggregate encoded-byte pages before materializing a page. Claim projection excludes later accepted turns, and
incremental context compaction keeps resident history proportional to the
configured model context rather than session lifetime.
Resume input is validated from the durable header before an idle session is
loaded, so rejected requests cannot reserve resident-session capacity. A claim
reader never merges the speculative suffix behind a durable watermark that
advanced during Store I/O.

Local contracts are safe-Rust, process-local authority. Session identities are
correlation values, not authorization tokens. Cross-process API, auth, RPC, and
browser control are outside this contract and must add their own trust boundary
instead of exposing these Local services directly.
