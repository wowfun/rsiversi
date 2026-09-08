# rsi-agent-session-protocol

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
headers (format version 9), bounded identities, append-only Facts, and one terminal outcome per
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
