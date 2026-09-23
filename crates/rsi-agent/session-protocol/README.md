# rsi-agent-session-protocol

Reference envelopes use version 2. Source identity distinguishes native Header
bindings from observed product records; capture metadata distinguishes a bounded
suffix from an exact selected record, content kind, range and cutoff. These are
user-data provenance, never execution authority or evidence of an external agent's
durable acceptance. Only the source owner can reread and freeze a selection.

Format 16 freezes optional `DelegationPolicy` in child Headers: selected role,
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

`FrozenAgentSettings::validate_policy` validates the non-routing fields for
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

Command invocations bind a ContributionId, DomainRequestId, typed draft or
durable control revision, and at most 16 KiB of structurally validated JSON
arguments. A durable command control retains that complete invocation alongside
its domain replacements; the canonical request digest therefore binds command,
arguments and expected revision. Its request identity must equal the control's
request identity, and a draft revision cannot appear in a durable command.
Command controls contain no execution Facts and cannot claim a Turn's free
mutation lane. Their consumers obtain Session authority through the owning
Kernel service; serialized identities alone confer no authority. Only Header
format 16 is accepted; all other formats are unsupported. The
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
headers (format version 17), bounded identities, append-only Facts, and one terminal outcome per
turn. It is a data contract, not a Runtime service or transport.

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

Each immutable settings value carries a `TurnBudget`, with repository hard maxima
of 30 elapsed minutes, 64 provider attempts, 256
Tool calls, 65,536 generated records, and 64 MiB of generated record bytes.
Generated records include ordinary generated Facts and Turn-attributed domain
controls, charged by their complete canonical envelope. Baselines and external
commands use separate bounded admission; necessary atomic ending records retain
the Kernel-owned ending channel. Settings may only tighten these limits.
Budget exhaustion is itself a nonterminal Fact
followed by the sole `budget_exceeded` terminal outcome, so interrupted
observers and recovery can classify the stop from durable history.
Both records validate that their frozen limit is positive and no greater than
the hard maximum for the named dimension; foreign history cannot widen a
budget while claiming it was exhausted.
The budget is mandatory in the current durable header encoding; decoding never
widens an omitted budget to repository maxima.

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
