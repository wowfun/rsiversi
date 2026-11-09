# rsi-agent-session-protocol

`StepPrepared.instructions` is an optional complete ordered instruction update.
Absent means retain the current Step's instructions; an empty list clears them.
Step start/end resets them. Tool selection and application capture remain explicit
on every preparation, independently of instruction equality. Format 36 owns
these update semantics; older formats are rejected rather than interpreted as
the current contract.

`TurnOutcome::Blocked` records an input-preparation rejection. It is distinct from
cancellation, provider failure and successful completion; no provider request is
authorized by the rejected preparation. Initial `enter([])` completes normally.

`StepInputDecision` describes a final ordered input decision independently of
mailbox acceptance. Enter may preserve, reorder, replace or omit claimed messages,
and may add attributed user/developer messages. Reject is distinct from entering
an empty list. A claim coordinate is a nonzero local Fact sequence and its message
ordinal; the execution owner must resolve it against the current captured input,
not arbitrary historical Facts. Reusing a coordinate in the output does not claim
or consume its mailbox receipt twice. Unchanged messages retain coordinates only,
avoiding another copy of accepted text or attachments.

Prepared messages preserve their original presentation identity and metadata;
these are not execution authority. Empty blocks and empty message content are
valid preparation results even though fresh ingress requires meaningful input.
The serialized entered list has a 32 MiB prepared-input envelope. Typed
construction and deserialization validate it; safe-Rust consumers borrow it
without repeating serialization. Session format 36 retains verbatim files as
typed `FileRef` content, independently of image and conversation references.
Image blocks may include optional display metadata independent of the raster identity. File bytes and provider-local paths do not enter Facts. It separates `InputMessageClaimed`
and `ToolContextsClaimed` from `StepInputsPrepared`. Direct `TurnAccepted` text is
also a claim at ordinal zero of its acceptance Fact. A preparation consumes the
owning Turn's current claimed set and records its final input decision. A prior claim uses its absolute Fact sequence. A context input published in the same batch can use a positive distance back from the preparation Fact. This keeps the message coordinate stable when another writer advances the Session during a callback. A claim
reference cannot name another Turn or an already consumed preparation. An unchanged
input keeps its source role. Agent instructions, the skill catalog and ordinary
plugin guidance use developer; Tool-origin context and other inbox input use user.
An authored or replaced message carries its own final role. Only the
final decision enters model history. The complete activation is tracked in
[the proposed input-preparation decision](../../../.agents/notes/proposed/architecture/2026-11-04-claimed-and-prepared-input.md).

Entered tool context identifies the original Session, effect, result ordinal,
message ID and source metadata. Its PluginContext provenance remains distinct
from human input when a tool supplies a user-role model message.
One or more deferred tool messages entering the same Step share one
`ToolContextsClaimed` Fact. Its ordered entries each carry their own plugin
attribution and original text blocks. The bundle is bounded to 4,096 messages
and 16 MiB; Store paging and append limits remain independent of message count.

The Session format records inbox suspension independently of message horizons.
`InboxPaused` prevents the pending next-Turn mailbox from starting execution;
`InboxResumed` restores its eligibility. Neither record removes or rewrites a
message, changes a steering binding, or belongs to model conversation context.
New Sessions start unpaused. Kernel owns when a waking operation resumes the
inbox; queue replacement, non-waking injection and terminal promotion do not
implicitly resume it. Both transitions replay from the durable control log.

The format retains optional original message attribution on command and
direct application/user inputs, independently of their native identity and
provenance. Attribution has a nonempty safe-text ID of at most 256 UTF-8 bytes and structurally bounded JSON
source; the complete attribution is at most 16 KiB of compact JSON. The same
attribution and ordered text blocks enter the consumed input Fact and survive
independent history replay and forks. No attribution value grants authority.
Direct ingress records `Human` for user actions and `Application` for unattended
submissions. Both reach the model as user messages. Only the former supplies
initial human authority. Origin and attribution are part of the exact accepted message. Changing either while
reusing a native message ID conflicts instead of changing the prior acceptance.

`StepPrepared` records the optional application source.
A ready source records its process generation and the admitted tool names,
application registrations and native result owners. The captured runtime owns
the schemas used for execution; `ModelIntent` request evidence owns their durable
inspection representation and may reference an earlier identical section.
`StepPrepared` does not persist a second schema catalog. When request evidence is
unavailable, schema inspection is unavailable; registration routing and cold
outcome recovery still use the complete captured identities. An unavailable selected
source records its diagnostic. Absence denotes no selected application.
`ToolIntent.identity.owner_id` binds an application call to the captured row.
Sources with durable outcomes also record their stable receipt source on the
ready view. An absent source carries no authority to look up an external result.

Tool context messages originate in a ToolResult or in the ToolRejected Fact
of a model call whose policy returned deferred messages. A refusal with no
messages omits the optional field. Each claim records its
source Session, effect and message ordinal. Plugin metadata grants no Human
input authority.

Command-produced mailbox input carries `Command` provenance: its registered
contribution identity, exact request identity, invocation digest and output
ordinal. It is never classified as direct Human input. This serialized provenance
grants no dispatch authority; only the command owner may admit it, atomically with
its state change (or with the first baseline for an unpublished draft). Text is
bounded like other non-human Agent messages and cannot carry execution overrides.
Command inputs retain one or more typed text, image or file blocks in their original
order. Their external admission owner verifies referenced objects; the protocol
validates descriptors, provenance and aggregate bounds without filesystem I/O.
Session format 36 admits this content under both pending and claimed command
provenance. Command input grants no execution overrides or frozen-reference authority.

`ExtensionJournal` is the built-in version-1 envelope for a trusted process
extension's event journal. It is one typed domain (`rsi.extension-journal`),
not a second Session store. Immutable Agent-CAS segments hold extension data;
the bounded domain records their contiguous event ranges, exact content hashes,
artifact identity, and an optional recovery checkpoint. Checkpoint JSON cannot
carry implicit Agent references: every retained CAS or Session edge must occur
in the envelope's typed reference fields. Up to 256 segments and 64 MiB of
segment bytes are admitted; a consumer must reject capacity before effects.
The Store understands these reference semantics without an installed extension.
Execution still requires the original artifact and its semantic adapter.

`MAXIMUM_LIVE_TURNS` bounds concurrent nonterminal Turns in one Session. Kernel
admission and durable timing projections share this cardinality limit.

Live prefix transport appends canonical base64 pages at an exact raw byte offset.
`ModelPrefixDecoder` retains immutable compact blocks and a grammar cursor, validating
only newly appended events. Each revision ends at an event boundary and declares
the complete event count; gaps, rewrites, malformed events and overflow fail closed.

The version-1 compact attempt stream contains unsigned minimal LEB128 time gaps,
a byte tag, an index for delta tags, a minimal LEB128 payload length and payload.
Tag 0 carries canonical normalized event JSON; tags 1/2/3 carry UTF-8 text,
reasoning or Tool-argument deltas. Canonical padded base64 blocks split the raw
stream at 147,453 bytes; only the final block may be shorter. The event reader
checks block lengths while decoding, using strict canonical padding and zero
trailing bits. Durable admission performs this decoding once during grammar
validation, without a separate decode/re-encode pass. Readers enforce the same
upper bound on the final block. Raw suffix offsets must fall within the
validated prefix; out-of-range programmer input is rejected before indexing.
Readers preserve the original event index and elapsed time within its single settlement Fact.
Payload extraction copies contiguous decoded slices across block boundaries;
it preserves the same raw cursor and canonical-record validation without a
per-byte dispatch loop. Cross-language consumers use the Rust-owned fixtures.

`ModelAttemptSettled` is the only durable language-output record. Its canonical
version-1 compact stream preserves every accepted normalized event, original
fragment boundary and elapsed millisecond offset. Text, reasoning and argument
deltas use tagged UTF-8 records; other events use their exact normalized JSON.
Unsigned integers use minimal varints. Canonical base64 blocks are at most
256 KiB including JSON framing and are decoded incrementally. The complete
settlement body is limited to 32 MiB, including 64 KiB reserved for a terminal
event. Capacity failure cancels the provider and settles the accepted prefix
with `model_output.capacity`; it never silently drops evidence or authorizes
tools. The existing 36 MiB Fact and 64 MiB Store transaction bounds remain.
Exactly one settlement must match a started effect, including after controlled
cancellation, before Step/Turn termination. A crash leaves no fabricated
settlement. Intermediate retry attempts have `retain_context: false`; final
partial text/reasoning is retained with an interruption marker and no tool
authority. Request, source, usage and replay evidence are projected from this
single canonical stream.

Format 23 gives every frozen reference snapshot a bounded `capture_id`, independent
of its content address, so pending captures can be released independently.
It also freezes `FrozenAgentSettings.output_budgets`: at most 256 unique
exact deployment/model entries, 64 KiB encoded, each with a validated
`max_output_tokens`. An empty table uses the selected model's declared default request cap.
Model/effort selection never changes this Session policy.
The immutable table validates at construction and deserialization; enclosing
settings validation trusts that typed value without reserializing it.
Within the current format, omitted output policy means an empty output budget
table. Serialization emits that table explicitly.
These policy defaults do not admit any earlier Header format.

Historical inheritance and execution ownership are distinct. A history origin
binds one source Header, a `before_turn` or `through_turn` boundary, the selected
completed local Turns, and exact Fact/control prefix digests. Agent spawn adds
tree membership and a live creator's execution owner; a manual fork has historical
origin only and starts an independent execution root. History links retain their
source Session without becoming parent/child control authority. Full traversal is
bounded to 32 source Headers; Last and None stop that traversal.

Reference envelopes use version 2. Source identity distinguishes native Header
bindings from observed product records; capture metadata distinguishes a bounded
suffix from an exact selected record, content kind, range and cutoff. These are
user-data provenance, never execution authority or evidence of an external agent's
durable acceptance. Only the source owner can reread and freeze a selection.

Headers require complete execution coordinates: a Local or stable SSH location
and its target-canonical workspace path. Decoding validates both together without
filesystem I/O; a path alone never selects an execution machine. Forks preserve
the exact parent coordinates. These coordinates are durable identity, not a grant
or a reconstructible execution lease.

The Header freezes optional `DelegationPolicy` in child Headers: selected role,
persona, normalized role digest and at most 64 ordered effective Tool names.
This records a monotone restriction, not a permission grant or an executable
generation pin. Initial structured-output contracts are optional;
follow-up activations do not inherit their schema. Headers carry the selected
canonical workspace without a workspace-trust field. Header fingerprints and
fork bindings include the complete Header. All other format versions are
rejected before validating current Header fields, without migration or file
rewriting. Current Headers reject unknown fields.

Human message content may include a frozen Session reference. The Agent-owned
reference binds a bounded preview and immutable CAS envelope to the source and
original target Header fingerprints. It is user data, never instruction authority.
At most four references enter one message; each preview is at most 8 KiB and
counts against the human text limit. The immutable envelope retains at most 1 MiB
of exported text, its exact source horizon and omission metadata. Reference reads
use recorded Session/Fact/content-index coordinates, never an arbitrary digest.
Protocol validation establishes shape and size only. The
[reference owner](../references/README.md) verifies CAS bytes, digest, metadata
and the exact preview prefix before admission or a read; protocol validation
alone is not proof of that binding.

`AgentPath` owns its compact JSON number-array encoding and
`AgentPath::MAXIMUM_JSON_BYTES`. Indexed storage consumers use that bound rather
than deriving a second limit from the path's depth and segment representation.

ModelIntent request evidence is an atomic Available or Unavailable package.
Configuration, system and tools sections hold UTF-8 inline bytes or an exact
earlier same-Session inline section reference (sequence, section, digest, length).
References never chain. Decoded section bytes total at most 16 MiB; ordinary
content is represented only by a bounded typed count/byte manifest. The Kernel
separately admits at most 16 MiB new inline bytes per Turn, within its existing
generated-byte budget. Fact construction validates evidence with its final sequence
in one pass; earlier direct-reference targets are checked by Kernel and grouped
by source sequence, so shared originals are read once. Optional evidence cannot trigger a second provider Prepare.

`FrozenAgentSettings.pricing` captures at most 256 exact deployment/endpoint/model
quotes, 64 KiB encoded and eight currencies. Rates are unsigned integer currency
billionths per token. Optional cache rates replace that input subset and require
its measured count. ModelIntent freezes the matching quote; Kernel validates it
against the immutable Header. Empty pricing means cost is not configured.

`FrozenAgentSettings::validate_metadata` validates identity and prompt for
configuration owners that have not selected a model yet. It does not construct
durable settings. Constructors and deserialization still require a validated
model reference and the complete policy before creating `FrozenAgentSettings`.

## Derived extension snapshots

Session projection DTOs are disposable read values, outside the durable record
format. A snapshot binds the exact Session, Header fingerprint, composition digest
and either draft revision or a simultaneous durable Fact/control horizon. Each
registered producer contributes one complete value or one bounded failure. There
are at most 64 unique producers, each value fits 64 KiB of canonical JSON, each
failure fits 4 KiB of diagnostic text, and the complete envelope fits 5 MiB.
Decode validates all of those bounds and identities. Consumers advance both
durable cursors monotonically; a durable view never regresses to a draft.

## Session commands

Queue withdrawal preserves the input's source and does not cancel a claimed Turn.
Only replacement and steering conversion require Human provenance. The initial
input of a ProgramRun-owned child requires Program cancellation rather than plain
withdrawal; its execution owner, not the message source, determines that relation.

A `DomainStateCommit` contains domain replacements, command-produced inputs, or
both. An input-only commit is valid only for a command or first-publication
baseline. Empty commits remain invalid; Turn and continuation domain mutations
still require replacements. The digest covers the complete input batch even when
the domain set is empty. Removing the last input cannot produce an empty commit,
including through deserialization.

Command invocations bind a ContributionId, DomainRequestId, typed draft or
durable control revision, and at most 64 KiB of structurally validated JSON
arguments. A durable command control retains that complete invocation alongside
its domain replacements; the canonical request digest therefore binds command,
arguments and expected revision. Its request identity must equal the control's
request identity, and a draft revision cannot appear in a durable command.
Command controls contain no execution Facts and cannot claim a Turn's free
mutation lane. Their consumers obtain Session authority through the owning
Kernel service; serialized identities alone confer no authority. Only Header
format 36 is accepted; all other formats are unsupported. Header protection is
part of this format's immutable shape. The
[SQLite contract](../store-sqlite/README.md) owns the exact database version;
earlier authoritative formats are rejected without rewriting their files.

Client command receipts are compact validated projections of canonical command
controls or lease-local draft mutations. DraftChanged binds the successor draft
revision and complete baseline digest; Committed binds the exact control cursor
and canonical domain request digest. Both retain command/request identities and
the original invocation digest. They do not copy complete domain states into
transport receipts or create another durable receipt store.

## Execution records

Each ModelIntent freezes its full purpose. Each ModelEvent repeats only the
small Conversation/ContextCompaction kind, which Kernel checks against the
intent. Partial history pages therefore identify internal summary output even
when they omit the earlier intent. Only the intent's validated plan plus its
Finished output can install a summary; the event tag grants no new authority.

`PluginContext` attributes actual model-visible text to one validated
ContributionId. It is text-only, uses the entered-message byte/block bounds,
and carries the exact open Step identity. Kernel accepts it only at a boundary
without an active external effect. Context builders replay it as developer
content; they do not resample or rerun its producer.

`ToolRejected` records an exact prepared Tool call denied before intent/start.
It preserves name, arguments and identity plus either a denied live approval
outcome or a bounded contribution-attributed policy reason. It counts as one
Tool call and one generated record. A rejection is not a retained Tool result,
never creates a started effect, and cannot replace an already admitted intent.
Context replay emits the corresponding error Tool response. Approval denial
retains the executor's existing failed-Turn behavior.

Complete domain values use a bounded JSON envelope (256 KiB, the shared JSON
depth/node limits), an exact identity/version and a checked revision. JSON null
is an ordinary explicit state, distinct from absent revision zero. A frozen
baseline contains at most 64 domains and 1 MiB of complete-state bytes. These
mechanical bounds do not replace the owning domain's typed semantic validator.

This package owns the exact pre-release durable Session format: immutable
headers, bounded identities, append-only Facts, and one terminal outcome per
Turn. It is a data contract, not a Runtime service or transport.

Canonical workspace paths in Headers and Facts describe their originating host.
They use the [Workspace host-path grammar](../../rsi-workspace/path/README.md).
Decoding never asks the reader's native filesystem whether that foreign path is
absolute. This lexical validation grants no filesystem authority; the native
Workspace, context and process owners validate the actual directory they use.

Agent control records form a second append-only digest chain beside Facts.
They own mailbox acceptance/claim/discard, activation and wait transitions,
delivery-horizon promotion, completion reservations, and durable tree
scheduling signals. A pending non-waking next-Step completion or bound human steer which survives
its parent's activation Turn is explicitly promoted to a waking next-Turn
message before that activation can settle; ordinary fixed-horizon next-Step
messages remain held, and indexes never reclassify either without a canonical
control record. Model-visible
message entry remains a Fact: one atomic Store commit ties its control claim to
the exact activation, Turn, Step, and Fact sequence. A session may therefore
have a durable Header and control tail while its Fact tail is zero.
Mailbox depth, content blocks per message, and paths per workspace-touch Fact
have separate named 64-entry bounds. They currently share a value but are
independent contracts and may evolve without accidental semantic coupling.

Fork lineage records the parent Header fingerprint, tree path, invoking Turn,
resolved balanced completed-turn interval, and exact terminal Fact/control
sequences and prefix digests. The [Store contract](../store-protocol/README.md)
owns their atomic correlation. Fork
seeds retain provider replay events. The child has a new Session identity and
never mutates or truncates its parent's log. An effective-turn count of zero is
valid only for the exact empty interval whose Fact/control cursors and prefix
digests are all empty; every
nonempty resolved interval retains at least one complete Turn.

Every header carries one required `AgentPresetId`. Its lowercase
`[a-z0-9][a-z0-9-]*` grammar is also safe as a preset-directory segment, and
construction plus deserialization enforce the portable 255-byte segment bound.
The durable
value records which preset a session selected; process-local composition
generation handles are deliberately outside this format.

Turns have no cumulative elapsed, provider, Tool, record or byte allowance.
Settings reject the removed `turn_budget` field. `max_tokens` is a terminal
Step/Turn outcome and never authorizes automatic output recovery. Single
messages, Facts, frames and storage batches retain their finite capacities.

Language, Image, and Tool effects follow explicit intent/start ordering. A
direct Image request is durable before preparation, each successfully imported
image is committed as an ordered `MediaRef` Fact, and a later failure terminates
as `partial_failed` with those already-durable refs. Facts never contain media
bytes, resolved credentials, filesystem locators, or live capabilities.
Unconfined (`danger-full-access`) frozen settings are valid only when live
approval is required; this cross-field invariant is enforced on construction
and deserialization.

Custom deserialization revalidates nested protocol values, exact format,
identifiers, paths, diagnostics, Fact size, and sequence rules. Older formats
are rejected; this pre-release contract has no migration or compatibility
reader. A constructed immutable `SessionFact` retains its exact compact-JSON
length as an in-process validation proof; batching and Store admission trust
that proof instead of serializing the same typed value again.

The protocol also owns the canonical rolling SHA-256 chain over serialized
Facts. Context projection and Store append accounting share that algorithm but
derive it independently from their own Fact inputs, so an opaque checkpoint
cannot supply its own provenance proof.

Reference envelope admission allows six encoded bytes per text/preview byte plus
16 KiB of provenance. This includes worst-case JSON control-character escaping;
decoded text and preview retain their independent 1 MiB and 8 KiB limits. Draft
and recorded page requests share `validate_reference_page_bounds`.

Initial structured output is frozen with the spawn message identity in the child
Header. It does not apply to follow-up activations or descendants. The accepted
value lives once in its ToolResult Fact; conclusion metadata, the Turn terminal,
ActivationSettled and the parent's Completion carry bounded references only.
References bind child, activation, Turn, exact Fact sequence, schema/value digests
and a 2 KiB preview. Exact parent Completion lookup proves final successful
activation settlement before the Kernel reads the referenced Fact. No latest
result or history scan is authoritative. The complete encoded Completion message
must fit the existing 8 KiB reservation.

Output schemas use the local Draft 7 validator with remote resolution disabled
and the DSH finite, object-root subset. Schemas are at most 64 KiB, values 256 KiB;
refs and unrecognized keywords are rejected. Missing valid output at natural
completion fails with `structured_output.missing`, without a synthetic retry.

`DelegationPolicy::role_sha256` identifies normalized requested role configuration
for exact spawn retries; it is not a digest of the effective Tool intersection.
The immutable Header fingerprint covers the effective policy, including its Tool
set. Durable Store ownership/integrity is required; this is not a signature against
a party capable of rewriting both Header and fingerprint. Current catalogs and
ancestor restrictions still intersect the frozen set before new admission.

Output contracts validate schema structure and Draft 7 syntax at construction;
they compile the value validator lazily once per shared contract, only when used
to validate a result. Bounded encoding computes digests and a preview without
materializing the complete encoded value. Digests identify exact compact JSON
bytes, including object key order; contract equality uses the same identity.
Process-local clones share the lazily compiled value; serialization remains schema-only.
Schemas permit at most 64 total `oneOf` branches
across the complete schema. Value rejection identifies the first failing instance
and schema JSON pointers (each capped at 256 characters), without embedding the
rejected value. Summarization encodes the bounded value once for validation size,
digest and UTF-8 preview.

`ToolCallsSuperseded` names a Turn and its completed Conversation model effect.
It closes the still unadmitted, unrejected calls when new NextStep input enters;
it is not a Tool result or a prepared Tool identity. Kernel records it atomically
with Step transition and input consumption. Terminal histories may still retain
missing outcomes; Context owns their provider-facing explanation.

Named spawn definitions are recorded in the child Header as a validated complete
role seed, source digest and immutable original-request digest. This seed is
separate from the effective DelegationPolicy, which intersects ancestor Tool
restrictions. A child never inherits its parent's spawn receipt or rereads the
definition during restore. Kernel owns fresh resolution versus exact retry.
Headers without a named spawn omit this optional field, preserving their canonical
encoding and existing fingerprint.

ToolIntent records an explicit `ToolOrigin`: Model binds the completed provider
effect, while Program binds an exact enclosing coordinator Tool effect and a
positive call ordinal. It also records the sealed Tool's program role for cold
validation. A program-origin intent must be Callable. Model-source supersession
remains specific to Conversation calls and cannot supersede program calls.

Program entered-input Facts obey the same single-text and complete 8 KiB encoded
notice bound as their mailbox message; entering a Step cannot broaden content.

Queue mutation fingerprints hash the canonical closed envelope while counting its
encoded bytes in one pass. Shape and content limits apply before hashing; the
streaming writer enforces the same request byte limit without retaining JSON.
## Step Tool selection

`StepPrepared` records the bounded, sorted names admitted by the complete
BeforeStep stage within one exact open Step. A durable Step may contain multiple
model retry series; each preparation replaces the previous one after its effects
and Tool calls have settled. It precedes provider dispatch and is
committed with that stage's context inputs and domain replacements. Absence means
the complete frozen catalog. It is execution metadata, never model-visible text.
Provider retries and Tools produced by that request retain its selection.
It also records up to 64 producer-bound instruction sections, at most 64 KiB in
aggregate. They apply until the next preparation or Step boundary; they
are not appended as historical conversation messages.
