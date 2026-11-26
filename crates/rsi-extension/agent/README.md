# rsi-extension-agent

The journal adapter stages bounded immutable event segments in the Agent Store
and returns a typed domain proposal. It retains each prepared CAS object until
the caller supplies the exact Kernel receipt or proves rejection. It does not
publish Session state or decide business-operation retries.
After a valid staged identity, local command-input or journal preparation failure
sends a rejected operation receipt before releasing its unresolved ownership.
Tool, Step and command receipts accept only null or the exact unloaded response;
a missing or invalid acknowledgement retains the uncertainty obligation.
BeforeStep receipts verify guidance only in `StepPrompt::AppendToHeader`; a native
complete assembly is a different preparation owner and cannot acknowledge the
extension's staged guidance. Omitted same-Step updates retain their existing meaning.
The owner commits its proposal together with the matching ToolResult or complete
contribution stage. A receipt must contain the exact proposed snapshot and CAS
predecessor; a matching public operation name alone is insufficient.

The built-in `ExtensionJournal` reference envelope is owned by the Session
protocol. Tool restoration and resource reads select the journal domain directly
at the current durable control cut. Event interpretation and checkpoint schemas belong to the extension.
Ordinary segments target 256 KiB of JSON. A single larger event occupies its own
segment within the journal's existing 64 MiB total and Store object limit;
journal range, byte and segment limits are checked before CAS publication.
CAS event bytes are not domain checkpoint values. Restores authenticate each segment's
hash through Store reads and check its count before exposing any events.
Staging packs a new suffix into the last segment when their combined count and
encoded bytes fit. It authenticates that segment through the caller's read budget
and preserves event order and sequence positions; no event is summarized or
discarded. The replacement snapshot retains every referenced CAS object through
publication. Small operations therefore do not consume one permanent segment each.
Agent argument, journal and history encoders share one private bounded writer.
Inline argument admission counts JSON bytes without allocating a speculative
serialized buffer; larger arguments encode once into their retained buffer after
the bounded inline probe. The inline value copy remains owned by the transport,
while the original arguments remain available for exact publication verification.

The optional `AgentAdapter` executes an admitted external Tool under the
Kernel-issued caller authority. The generic `agent.load` / `agent.tool` wire
carries bounded journal bytes and an opaque extension checkpoint; interpretation
belongs to the external plugin.
Loading precedes business execution: a disconnect during `agent.load.*` is an
execution-preparation failure, not an uncertain Tool effect. Generation validation
still reports a closed connection as unavailable rather than incompatible state.
Its effect identity, arguments and prepared journal form one settlement candidate.
The pure settlement callback freezes the final result after the admitted native
around chain, including Spill retention. Repeated settlement must match that
same final result and identity. Executor's publication observer verifies the exact
Session, effect, result and domain replacement before acknowledging the process.
Unconfirmed operations remain fenced; runtime result retirement does not clear
them. Projection reads the committed checkpoint and performs no process I/O.
Before a later Tool, an exact canonical receipt can confirm a lost notification.
An absent receipt permits rollback only after the original Turn has a durable
terminal boundary and a second lookup still proves absence. Neither path replays
plugin execution. The journal records extension data independently of code selection;
the generation recipe owns the exact executable artifact. A replacement's generation
validator loads its complete committed journal before Kernel publishes selection.
Failure leaves the previous generation and durable journal unchanged.
Before transferring history, `agent.load.begin` identifies the Session, creation
time, current local working directory, domain revision and event count. For any
local Session working outside its creation directory, the Host also supplies
the creation directory. A directory change does not invalidate an otherwise
current journal. A resident,
quiescent mirror of this positive revision replies `current` and is retained
for the callback. The required `reload` flag forces transfer during candidate
validation, even for a revision already loaded by an abandoned candidate. This
preloads the proposed revision without claiming publication; after proven commit,
ordinary calls use `reload: false` and reuse that mirror. On `transfer`, the host restores each authenticated CAS
segment, sends ordered `agent.load.events` pages, then sends the checkpoint in
`agent.load.finish`. Draft revisions stay zero across edits and are transferred
in full. A remote execution
location supplies no local directory. A consumer needing a local path must
refuse that operation when the path is absent. The process publishes the
replacement mirror only after complete count and event validation. A page
does not acknowledge a durable mutation. Incomplete loads remain invisible and
consume the process mirror budget until explicit load cancellation or generation disposal.
A failed or dropped transfer owns `agent.load.abort` through a two-second
best-effort deadline, including cancellation during CAS reads. A bounded per-Session
load lane stays held through that cleanup so the next load cannot be aborted by
its predecessor. It covers only mirror transfer, not business callbacks or questions;
this releases only its unpublished pages. Cancellation or a disconnected peer
closes generation validation without labeling the journal invalid. Transport
capacity and Store CAS read pressure remain capacity failures, allowing a later
attempt after resources recover. Rejected content retains its diagnostic.
The Tool reply distinguishes a staged candidate from a definite rejection. A lost
reply retains the exact originating effect before the RPC starts. After its Turn
ends, canonical absence permits `operation.abandon` to discard only that effect's
tentative mirror. Neither a diagnostic string nor a missing reply proves rejection.

`AgentAdapterFactory` is an ordinary Agent-scoped plugin. It freezes the external
`agent.catalog` version-six Tool definitions and commands during activation, registers one
journal domain plus settlement/projection contributions, and retires registrations
with the generation. The catalog is bounded to 64 Tools. Adapter configuration is
null; its process connection already belongs to the selected executable generation.
The journal uses the version-2 built-in envelope, with no artifact field.
Packages require `rsi.extension.agent` version 10, so an older host rejects their
installation before selection; the generic process transport uses version 2.

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

During an executing extension Tool, `agent.tool.directory` checks or changes
that Tool's own Session directory. The Host binds both IDs to the live Tool
authority and uses the same directory and model-notice commit as an application.
History reads, fork transforms and closed operations carry no Tool authority.

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
For `model_attempt_settled` and the `settled` stage of `manual_compaction`,
the nested `attempt` contains `retain_context` and ordered
`(elapsed_ms, LanguageEvent)` values from the canonical decoder. The surrounding
Fact retains its canonical fields, including the command identity for an idle
compaction. Other rows retain their canonical representation. The encoded attempt chunks are not sent
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
One generation-wide budget admits at most 256 unresolved Tool, Step and Command
operations combined. A permit is reserved before dispatch, moves from unobserved
to prepared ownership, and is released only after definite rejection or confirmed
publication. Resource exhaustion never evicts an uncertain operation.
Separately, each `AgentAdapter` process generation admits at most 256 MiB of
retained Tool argument/result encoded weight. Before dispatch it reserves the
argument bytes plus the 17 MiB result ceiling; when settlement freezes the final
result it reduces that reservation to the canonical encoding of the typed payload,
not the peer's transport-body length (which may differ in whitespace or defaults).
The credit belongs to
the retained publication payload, including observer clones, and survives release
of the operation-count permit. This is neither a Host-wide memory limit nor an
account of the Tool runtime's separate returned result or JSON heap overhead.
A process may evict a settled mirror after its receipt, but must retain a newly
loaded mirror until its operation settles, plus active, queued, reviewing and
staged operations. A receipt for an evicted mirror returns `{"state":"unloaded"}`;
the Rust owner accepts that only after its normal exact canonical evidence check.
The next operation after eviction reloads the canonical journal. Eviction never authorizes
callback replay and does not discard an unresolved tentative suffix.

Transport refusal before writer-queue admission releases the exact new Tool,
Step or Command operation reservation without an abandon RPC. After admission,
lost responses retain their operation until ordinary publication reconciliation.
This distinction comes from the connection's typed request failure, never a
diagnostic string or the underlying error category.

After a staged reply, the operation owns one shared capacity retry for its
immutable result, event and Step-decision reads. A provably unsent read or local
byte-admission refusal may wait at most 10 ms and reattempt once, under the
original cancellation and deadline. Admitted read failures, disconnects and
business callbacks are never retried by this path. Exhausted read pressure keeps
the existing operation uncertainty and receipt obligation; peer staging is not
durability. Pure fork-transfer capacity remains a typed capacity failure.

Tool arguments retain their native 4 MiB JSON limit across process frames. Values
above 256 KiB use `agent.request-body.read` under the exact Session, operation
and owner-issued token. At most two request bodies are retained per generation;
each is released with its originating request. Tokens are monotonic identities
across all registrations in the native process, preventing stale token reuse when
an adapter is replaced on the same connection. The connection generation and exact
Session/operation binding provide their authority. Oversized argument JSON is an
invalid request, whereas occupied body slots are temporary capacity pressure.
The peer validates and reassembles
the complete body before executing the Tool. Cancellation and lost receipts keep
the existing operation-outcome semantics; a fragment read never executes a Tool.

Settlement events page from the existing staged operation under its exact token.
Normal pages contain up to 4096 events and 256 KiB; one larger event is carried
alone. Tool results are a separate inline or paged body, bounded to 17 MiB for
the bridge's empty-enforcement result: 4 MiB encoded value, 4 MiB encoded metadata,
at most 8 MiB escaped text, 64 KiB contexts and bounded image descriptors/framing.
No combined journal/result serialization is retained. `agent.operation-body.read`
reads these immutable staged parts; receipt or abandon retires their authority.
The existing 64 MiB native read budget admits transient encoded response buffers.
Event arrays decode one event at a time into canonical JSON, retaining no decoded
event aggregate. A separate per-adapter pool admits 64 MiB plus 256 KiB of retained
canonical received event-array bytes through CAS staging. The allowance covers
the extra array framing of at most 4096 individually encoded events; the durable
journal limit remains 64 MiB. Separate transient admission
lets staging authenticate an existing small CAS tail without competing with its
own retained suffix. That tail's Store read credit remains held through staging
its merged encoding. Both pools release on errors and cancellation. These are
encoded-weight limits, not exact heap limits: one decoded event and its canonical
encoding can coexist with its admitted source buffer. Reload and fork input above
256 KiB carry authenticated CAS bytes directly under the same retained Store
admission, without decoding and re-encoding the segment on the Rust side.
Smaller segments are decoded into the inline JSON envelope, avoiding another
fragment callback at the cost of parsing and serializing that bounded value.
These CAS transfers share the two request slots; their retained bytes remain
charged to the adapter's 64 MiB transient budget through Store read admission,
independently of the 4 MiB Tool argument limit.
The peer's existing 256 MiB mirror budget also admits staged encoded output and
fork output pages. Tool execution first reserves the larger of its known event
page and 17 MiB result bound; successful staging reduces this to the actual result
and event-page capacity. Only the currently requested result or event page is
encoded and cached. Later reads may reconstruct another immutable part without
executing the operation. Receipt, abandonment or generation disposal releases the
reservation; active reservations are visible to concurrent load and fork admission.

The generic `rsi.extension.journal` Session resource exposes a read-only head and
individual authenticated event segments. `head` returns the captured domain
revision, checkpoint and segment descriptors. A digest request is admitted
only while that segment belongs to this Session's canonical journal; it cannot
read arbitrary CAS data. A rewritten tail can invalidate an old head; clients
must reload the head instead of mixing histories. Reads use the Session resource
byte limits and no Node callbacks, and remain distinct from the 64 KiB projection.
The bounded decoded model-attempt projection is also consumed by Application
processes presenting an authorized Session history. Fact authorization and
cursor ownership remain with the caller supplying the canonical Fact.
