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
Preset Profile resolution uses one immutable Agent-only factory
snapshot acquired before each build. The same snapshot owns the preset compiler
and contribution resolver; source refresh cannot mix their authority. A source cannot name Store, Process, Jobs, Kernel, provider, Host, or
other global factories. Unknown or unsupported contribution identities fail
before a Tool stage is sealed or any session capacity is reserved.

Selected Session workspaces supply project `AGENTS.md`, skills and Markdown
Agent descriptions/personas by default,
alongside configured user instructions and skills. Discovery does not grant Tool
approval, Sandbox privileges, remote authentication or Workspace registration.
Selecting a workspace accepts its instruction and skill sources as model context,
including later edits and directory-link targets. A malicious checkout can
therefore influence model behavior; the product does not claim prompt-injection
isolation for those sources. Project skills take precedence over identically named
RSI-configured and personal skills: a checkout can replace what `/name`,
`/skill name` or `$name` resolves to, including a familiar personal skill name.
The selected logical source is shared by listing, preview and invocation.
The rationale belongs to the
[workspace discovery decision](../../../.agents/notes/implemented/simplification/2026-09-19-default-workspace-context.md).
Project instruction reads remain
relative to an owned project directory capability and do not follow symlinks.
Skill roots authorize directory-link targets, including outside the project;
skill files themselves must not be symlinks. Skill metadata and body reads use
the same resolved directory handle for each observation. Discovery, source
selection and read bounds belong to the
[workspace source contract](../workspace-context/README.md). An incomplete
observation cannot replace the last-good durable context.

Subagent control authority is process-local and claim-scoped. The executor
derives `AgentCallerAuthority` from the exact live claim and started Tool effect and injects it as a
typed Tool extension; model arguments carry only requested targets and cannot
name a root, claim seal, model-origin proof, or approval authority. Kernel checks
the ToolIntent against its completed source Conversation before issuing that
authority; settling the Tool invalidates it. Kernel lineage checks constrain
spawn, message, list, wait, and interrupt operations. The standard Session
adapter routes an approval answer only after validating the explicit owning
Session within the caller's durable Agent tree, then dispatches the exact
(Session, approval identity) tuple.

Program-origin Tool facts instead prove their exact still-started coordinator,
ordinal and frozen Local eligibility; they cannot forge a model call. Their nested
execution retains ordinary policy, approval and budget checks. Detached workflow
authority is a separate Kernel-issued owner with frozen model, permissions and
parent horizon. The [native program runtime](../program/README.md) uses
shell-equivalent Sandbox authority, with no JavaScript VM confinement claim.
Plan-mode changes revoke a captured workflow policy generation, including unknown
write acknowledgement; a completion notice cannot authorize a successor run.

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
canonical Facts. Open checks the exact schema. Header and recent-session reads
validate bounded metadata; explicit validation and execution/history access check
the selected session's relational and lifecycle consistency. The explicit offline verifier
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
