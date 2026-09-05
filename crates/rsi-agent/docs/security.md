# rsi-agent security

Durable and externally decoded values are validated by their owning protocol
before entering trusted runtime state. Session identifiers, canonical paths,
model routes, JSON values, Facts, Tool results, Media references, batch sizes,
and CAS bytes all have explicit finite bounds. Custom deserialization must not
bypass constructor invariants.

The durable Agent preset identity uses the same lowercase alphanumeric-and-dash
grammar as its eventual directory segment. It cannot contain separators,
dot-segments, absolute-path syntax, or an unbounded name; resolving that
identity to filesystem authority remains the preset provider's responsibility.
Preset Profile resolution uses a construction-time frozen Agent-only factory
allowlist. A source cannot name Store, Process, Jobs, Kernel, provider, Host, or
other global factories. Unknown or unsupported contribution identities fail
before a Tool stage is sealed or any session capacity is reserved.

Workspace trust is an immutable Header decision. Configured user instruction
and skill roots remain trusted inputs; project `AGENTS.md` and project skills
are eligible only for a trusted Session. Project discovery acquires one owned
root directory capability, then keeps enumeration, metadata, and source reads
relative to it. Project symlinks are omissions, not alternate authorities, and
an incomplete observation cannot replace the last-good durable context.

Subagent control authority is process-local and claim-scoped. The executor
derives `AgentCallerAuthority` from the exact live claim and injects it as a
typed Tool extension; model arguments carry only requested targets and cannot
name a root, claim seal, or approval authority. Kernel lineage checks constrain
spawn, message, list, wait, and interrupt operations. The standard Session
adapter routes an approval answer only after resolving one unambiguous pending
approval identity within the caller's durable Agent tree.

The session header records redacted configuration facts only. It may contain a
credential reference but never a resolved secret. Provider error summaries and
Tool failures are bounded before persistence. Media content remains owned by
the Media service; Agent Facts retain immutable references only.

SQLite owns files below its configured root and acquires an exclusive writer
lease before schema or recovery access. It rejects symlinked or non-directory
roots that cannot be canonicalized safely, and every SQLite database connection
uses the no-follow open flag after its path precheck. CAS publication writes new immutable
objects and verifies their digest; cleanup must never follow or delete a path
supplied by a session Fact.

The Store's derived turn rows are committed in the same transaction as their
canonical Facts. Open checks the exact schema; first access checks the selected
session's relational and lifecycle consistency. The explicit offline verifier
performs the whole-database physical, foreign-key, and logical audit while
holding the writer lease. Kernel recovery never trusts an index row without
decoding and validating the selected bounded Facts, while cold outcome lookup
uses a Store-validated acceptance/terminal boundary pair whose decoded sequence,
turn, and kind exactly match the selecting relational rows.

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
Checkpoint restore is additionally fenced by the claimed turn's acceptance
sequence, so an unfiltered maintenance checkpoint cannot swallow that
acceptance or project a later queued turn.
Resume input is validated from the durable header before an idle session is
loaded, so rejected requests cannot reserve resident-session capacity. A claim
reader never merges the speculative suffix behind a durable watermark that
advanced during Store I/O.
Every claim carries a process-local issuer seal plus an immutable binding over
its executor, claim, session, turn, Header fingerprint, acceptance, and live
horizon fields. Mutating a public projection cannot manufacture either live
claim authority or the post-terminal maintenance authority used for checkpoint
rebuilds.
The Kernel issues a move-only resume token only after pinning the resident or
current cold composition. The standard application obtains that token before
durably registering the Header's workspace, and the Kernel rejects tokens
issued by another service instance. The executor receives only the resulting
opaque resident pin. Neither module receives preset paths, Profile resolver
authority, or a mutable Tool registrar.

Local contracts are safe-Rust, process-local authority. Session identities are
correlation values, not authorization tokens. Cross-process API, auth, RPC, and
browser control are outside this contract and must add their own trust boundary
instead of exposing these Local services directly.
