# rsi-extension-agent

The journal adapter stages bounded immutable event segments in the Agent Store
and returns a typed domain proposal. It retains each prepared CAS object until
the caller supplies the exact Kernel receipt or proves rejection. It does not
publish Session state or decide retries.
BeforeStep receipts verify guidance only in `StepPrompt::AppendToHeader`; a native
complete assembly is a different preparation owner and cannot acknowledge the
extension's staged guidance. Omitted same-Step updates retain their existing meaning.
The owner commits its proposal together with the matching ToolResult or complete
contribution stage. A receipt must contain the exact proposed snapshot and CAS
predecessor; a matching public operation name alone is insufficient.

The built-in `ExtensionJournal` reference envelope is owned by the Session
protocol. Tool restoration and resource reads select the journal domain directly
at the current durable control cut. Event interpretation and checkpoint schemas belong to the extension.
One segment contains at most 256 KiB of JSON; journal range, byte and segment
limits are checked before CAS publication. Restores authenticate each segment's
hash through Store reads and check its count before exposing any events.
Staging packs a new suffix into the last segment when their combined count and
encoded bytes fit. It authenticates that segment through the caller's read budget
and preserves event order and sequence positions; no event is summarized or
discarded. The replacement snapshot retains every referenced CAS object through
publication. Small operations therefore do not consume one permanent segment each.

The optional `AgentAdapter` executes an admitted external Tool under the
Kernel-issued caller authority. The generic `agent.load` / `agent.tool` wire
carries bounded journal bytes and an opaque extension checkpoint; interpretation
belongs to the external plugin. Its retained result, effect identity, arguments
and prepared journal form one settlement candidate. The pure settlement callback
only returns that candidate. Executor's publication observer verifies the exact
Session, effect, result and domain replacement before acknowledging the process.
Unconfirmed operations remain fenced; runtime result retirement does not clear
them. Projection reads the committed checkpoint and performs no process I/O.
Before a later Tool, an exact canonical receipt can confirm a lost notification.
An absent receipt permits rollback only after the original Turn has a durable
terminal boundary and a second lookup still proves absence. Neither path replays
plugin execution. The journal's cohort digest must equal the adapter's pinned
artifact identity before any event restoration or external callback.
Before transferring history, `agent.load.begin` identifies the Session, creation
time, local working directory, domain revision and event count. A resident,
quiescent mirror of this positive revision replies `current` and is retained
for the callback. On `transfer`, the host restores each authenticated CAS
segment, sends ordered `agent.load.events` pages, then sends the checkpoint in
`agent.load.finish`. Draft revisions stay zero across edits and are transferred
in full. A remote execution
location supplies no local directory. A consumer needing a local path must
refuse that operation when the path is absent. The process publishes the
replacement mirror only after complete count and event validation. A page
does not acknowledge a durable mutation. Incomplete loads remain invisible and
consume the process mirror budget until explicit load cancellation or generation disposal.
A failed transfer sends `agent.load.abort`; this releases only its unpublished pages.
The Tool reply distinguishes a staged candidate from a definite rejection. A lost
reply retains the exact originating effect before the RPC starts. After its Turn
ends, canonical absence permits `operation.abandon` to discard only that effect's
tentative mirror. Neither a diagnostic string nor a missing reply proves rejection.

`AgentAdapterFactory` is an ordinary Agent-scoped plugin. It freezes the external
`agent.catalog` version-five Tool definitions and commands during activation, registers one
journal domain plus settlement/projection contributions, and retires registrations
with the generation. The catalog is bounded to 64 Tools. Configuration pins the
artifact cohort; a durable journal from another cohort cannot be executed.
Packages require `rsi.extension.agent` version 8, so an older host rejects their
installation before selection; the generic process transport remains version 1.

Human questions require an explicitly installed Agent question channel. Each
outbound Tool call temporarily binds its exact caller and exclusive-final parking
authority. Reverse requests must match both Session and effect; they cannot name
another Turn or manufacture execution authority. Closed review bindings hash the
exact question document and caller identity. The Kernel parks during the wait and
resumes before returning the validated answer. A dismissed review is an explicit
closed choice; Turn cancellation remains cancellation. Dropping a call or handler
withdraws admission and cancels pending questions.

BeforeStep uses the same retained journal and receipt protocol as Tool settlement.
Its framework-assigned operation identity exists before the external call begins.
The committed receipt identifies the selected Step and a Tool subset within the
proposed set. Explicit instruction updates contain the producer's staged text;
an omitted update relies on the executor's equality check against that Step's
canonical Context assembly. The adapter trusts this typed in-process publication
contract instead of adding a second history lookup. Recovery reconciles an unknown
receipt against canonical history.

During preparation, `agent.history.read` borrows only that operation's captured
claim-visible Fact reader and horizon. Session and operation identities must match
an active scope. A history walk requests an inclusive end sequence and batches up to the native reader's page limit within the serialized Fact byte budget. An exact Fact lookup reads one position. The cursor includes skipped claim-invisible Facts. Ordered UTF-8 fragments (128 KiB each) carry one exact Fact page;
at most two serialized pages are retained across the generation. Large Facts keep
their owning 36 MiB bound without widening the process frame limit. Withdrawal
cancels reads and releases retained pages. This read port has no mutation rights.
Before a Tool callback, capture the started effect’s durable Turn prefix and expose the same operation-scoped reader used by BeforeStep. A plugin’s mutations must follow the recorded invocation even when no later Step runs. These observed Facts and the Tool mutation publish together with the ToolResult.

The extension records its consumed cursor inside the staged checkpoint, so rejection
or an unknown commit cannot advance history independently of the journal.
The BeforeStep history includes claimed tool messages with their durable Fact
coordinates, original IDs, sources and ordered content blocks. The regular
fragmented history reader also carries large batches across process replacement.
Before loading the plugin, the adapter checks that the journal head still has the captured revision. Journal publication uses that revision for compare-and-swap.
The request carries the native selection and any preceding preset decision. Its staged reply supplies an ordered input decision over captured claims. Downstream preparation may edit the entered list. The receipt must contain the final decision for the same Turn and Step alongside the journal commit. A staged rejection prevents later input preparation. Small decisions travel inline. Larger edits remain operation-scoped and cross the existing process frame boundary in UTF-8 fragments of up to 128 KiB. The host validates the reconstructed decision against the native preparation budget.
History pages are transient read views, not another durable Fact format. A
caller supplies the encoded page budget, from two bytes through the existing
serialized Fact ceiling. Encoding stops before the first Fact that would exceed
the budget; an individually oversized first Fact returns capacity. It never
returns part of a Fact or allocates beyond the supplied encoding budget.
Each `model_attempt_settled` row retains its sequence, timestamp, Turn, effect and
purpose, but its `attempt` contains `retain_context` and ordered
`(elapsed_ms, LanguageEvent)` values from the canonical decoder. Other rows
retain their canonical representation. The encoded attempt chunks are not sent
alongside that view, and there is no separate attempt-read callback. The complete
page stays within the existing 36 MiB ceiling, cutting before a later row when
needed; an individually oversized projection fails without truncation. The
extension neither implements nor gains authority over the compact binary codec.

The frozen catalog also admits at most 64 ordinary commands. Their canonical
arguments are a JSON string containing the slash-command tail, matching the
ordinary GUI and terminal submission contract. Command callbacks
use their captured journal and Fact horizon, and a separate staging lane so a
parked Tool question does not block command execution. Returned text is admitted
through typed command inputs. Draft acknowledgements prove the exact applied
receipt and complete initial publication digest; their journal revision remains
zero until first durable publication. Durable acknowledgements prove the exact
invocation, journal predecessor and input batch. Lost replies and acknowledgements
retain the owner's outcome handle: definite rejection permits abandonment, exact
publication permits acknowledgement, and unresolved outcomes remain fenced.
Neither operation replays a command to determine its outcome.

The journal requires an explicit fork transform. A captured parent journal is
authenticated into a separate paged read-only process transfer; it never replaces
the parent's live mirror. The process completes its history only through the
owner's terminal Fact cut, then returns a paged transformed journal and checkpoint.
Rust stages these into revision-zero CAS-backed proposals, retaining every object
and an explicit parent Session edge. No business callback or parent publication
runs. There are at most two temporary transfers; release is idempotent and an
unreleased transfer expires after 35 seconds, beyond Kernel's 30-second deadline.

Idle staging locks and confirmed Rust publication entries are reclaimed before
capacity admission. Unconfirmed and unobserved outcomes remain retained.
A process may evict a settled mirror after its receipt, but must retain a newly
loaded mirror until its operation settles, plus active, queued, reviewing and
staged operations. A receipt for an evicted mirror returns `{"state":"unloaded"}`;
the Rust owner accepts that only after its normal exact canonical evidence check.
The next operation after eviction reloads the canonical journal. Eviction never authorizes
callback replay and does not discard an unresolved tentative suffix.

The generic `rsi.extension.journal` Session resource exposes a read-only head and
individual authenticated event segments. `head` returns the captured domain
revision, artifact digest and segment descriptors. A digest request is admitted
only while that segment belongs to this Session's canonical journal; it cannot
read arbitrary CAS data. A rewritten tail can invalidate an old head; clients
must reload the head instead of mixing histories. Reads use the Session resource
byte limits and no Node callbacks, and remain distinct from the 64 KiB projection.
The bounded decoded model-attempt projection is also consumed by Application
processes presenting an authorized Session history. Fact authorization and
cursor ownership remain with the caller supplying the canonical Fact.
