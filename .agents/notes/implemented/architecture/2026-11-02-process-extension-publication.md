---
name: Process extensions retain proposals until canonical publication
---

## Problem

An external plugin can mutate its in-memory projection before RSI publishes the
corresponding ToolResult. Treating a successful RPC or Tool runtime retirement as
durability loses atomicity: retirement also occurs during cleanup. Reinvoking the
plugin after a lost acknowledgement can duplicate effects. A synchronous native
callback gate cannot carry independent reverse requests while another callback
waits for human input.

## Decision

The connection reports whether a failed request entered its writer queue. A
provably unqueued operation releases its new reservation locally; an admitted
request still requires the domain's receipt evidence. Treating encoding or
capacity refusal as an uncertain effect would fence a Session despite no peer
execution and retain capacity without a useful recovery obligation.

After execution, transient local read admission can obstruct settlement even
while the peer still retains its staged bytes. One shared bounded reattempt for
provably unsent reads allows that pressure to clear without replaying business
callbacks or extending the caller's deadline. Persistent pressure preserves the
receipt obligation: retained peer memory is not canonical durability. Sharing
the allowance across parts prevents large transfers from multiplying retry delay.

Large Tool arguments and Application submissions use request-owned, immutable
body bytes rather than widening the transport frame limit or introducing a
durable upload session. The same small body codec serves both real consumers;
each business owner retains its own authorization, byte budget and cleanup.
Fragment reads carry no execution authority, and the original invocation still
owns the single execution and receipt. Domain protocol versions reject an old
peer before it can misinterpret the new body envelope.

Native-sized Tool input also becomes journal history and may produce a large
result. Paging only its request would let execution succeed before an unrelated
reply or checkpoint-value ceiling prevented settlement. Journal segments therefore
use the Store object's own bounded encoding, while staged result and event pages
reuse the existing operation token and receipt lifetime. The peer charges only
one encoded part at a time to its existing mirror budget; large CAS reload and
fork input borrow authenticated retained bytes instead of decoding and re-encoding
them. Small segments keep the inline JSON path to avoid another round trip.
A random staged token identifies the retained operation; canonical result
and publication equality, not a redundant hash of the entire reply, proves receipt.

Operation-count admission alone is insufficient when a retained Tool result can
be much larger than a process frame. The native adapter reserves worst-case
argument/result encoded weight before dispatch, then shrinks to the actual typed
payload. This credit follows the publication payload, not the independently
released operation slot; an observer may still retain that payload after receipt.
The scope is one adapter generation, and a returned Tool runtime clone remains
that runtime's owner. The peer mirror policy informs the native aggregate budget
without claiming an exact heap or whole-Host memory cap.

Opaque event arrays need no aggregate decoded tree in the native adapter. Decode
one event at a time into canonical JSON and pack the encoded arrays independent
of transport page boundaries. Persisting raw peer JSON instead would change CAS
hashes when whitespace, key order or number spelling differs. Received canonical
bytes retain separate aggregate credit through CAS staging; a merged old tail
keeps its existing Store read admission through the derived encoding's staging.
Separating transient read ownership from received suffix ownership allows a full
valid journal to authenticate its small tail without waiting on its own credits.

Request-body tokens use a monotonic serial shared across native registrations.
Their exact connection generation, Session, operation and live-scope checks grant
read authority; unpredictability adds none for this already authorized peer.
Process-wide uniqueness matters because an adapter can be replaced on the same
connection. An instance-local counter would let an old token name a new body.

The generic [process connection](../../../../crates/rsi-extension/core/README.md)
owns bounded duplex framing, generation identity, cancellation, native settlement
and a liveness watchdog. It runs explicitly trusted native-equivalent code. The
optional [Agent adapter](../../../../crates/rsi-extension/agent/README.md) owns
retained Tool settlement over the Agent's existing atomic domain commit.
Meta has no knowledge of extension event types or DSH services.

Installed process closures use PackageStore's bounded metadata admission and
lifetime leases. Launch authenticates the descriptor and resolves the paths it
uses. Full content verification belongs to installation and ordinary acquisition,
as specified by the
[package contract](../../../../crates/rsi/packages/README.md).

The published Better Sidebar resolves explicit filesystem paths with the
host user's permissions, including paths outside the Session's default
directory. A marketplace can also install executable code. Applying a path
filter to individual requests would leave other plugin operations with the
same privileges. The [application forwarder](../../../../crates/rsi-api/http/README.md)
therefore admits the browser identity issued by the local launch owner before
calling the plugin. Ordinary paired devices keep their existing API grants.

A resident listener keeps that launch identity across application crashes.
Each process owns its own key, endpoint and physical request handles. A new
request can wait for replacement while a request already sent to the failed
process keeps its original outcome. This lets the browser recover without
giving the replacement authority over an old mutation, model observer or
terminal writer.

Pure settlement produces typed proposals and retains a separate publication
observer. Executor verifies the exact canonical result and domain receipt before
notifying that observer. Dropping an unfinished notification preserves uncertainty.
The adapter reconciles exact receipts; absence licenses rollback only after the
originating Turn's terminal fence and a second request lookup. No path invokes the
Tool again to establish what happened. The
[composition contract](../../../../crates/rsi-agent/composition-protocol/README.md)
owns this interface, preserving the atomic publication decision in
[execution contributions](2026-09-08-execution-contributions.md).

The built-in Session journal envelope has explicit CAS and Session roots so
offline Store reference maintenance does not require an installed plugin. Its
event and checkpoint semantics remain extension-owned. It uses ordinary typed
domain proposals; it does not restore arbitrary JSON writers rejected by
[typed domain commits](2026-09-08-typed-domain-commits.md). A projection reads the
committed domain checkpoint without I/O. This is extension state, not a framework
projection cache; [pure projection ownership](2026-09-09-session-extension-projections.md)
is unchanged.

Code selection belongs to the immutable generation recipe, not to a second hash
inside the extension's journal. A replacement validates and loads the committed
journal through its generation validator before Kernel can publish selection.
The existing paged load accepts an explicit reload request for speculative
candidate preparation, so a repeated proposed revision cannot reuse different
abandoned state. Publication makes that prepared revision usable by ordinary
callbacks without a second full history transfer. Failed candidates retain no
authority over the previous executable pin or durable state.

BeforeStep retains the existing atomic Step acceptance. One RSI durable Step may
contain several provider retry series, so preparation may replace current system
guidance and narrow Tool declarations after outstanding effects and calls settle.
The exact assembly is a durable `StepPrepared` Fact; a checkpoint retains it only
while that Step is open. Tool origins retain the selection of their producing
request, including nested Program dispatch. Historical PluginContext messages
cannot represent current Plan instructions: they would keep an exited policy in
later requests. This refines the execution-contribution representation without
sampling unrecorded guidance inside the Context builder.

The native Plan Policy uses that same instruction slot. DSH's `plan:policy`
section supplies the selected policy to each current prompt; repeated unchanged
preparations do not create additional policy messages. Appending native policy
text as PluginContext would also change the aggregate system evidence every
request, multiplying old copies in durable request inspection. Reusing the slot
requires neither another state cache nor another durable policy owner. The typed
plan domain and Tool policy retain their existing authority.

Time context follows DSH's committed-event refresh model: the default ten-minute
interval is measured against entered input, so failed callbacks cannot suppress
the next sample. The existing Context fold supplies per-producer timestamps
without rereading history. DSH retains its last injection time across Session
history; native markers share retained Turn lifetime, so dropping that Turn
permits a fresh clock sample.
Adding a business domain just for a refresh timer would add commits and change
complete Session domain baselines; a callback-local cache would lose recovery
and fork semantics. Neither is needed for derived input metadata.
The fold cache format changes independently of the builder's message semantics.
Keeping the semantic identity avoids rejecting existing, still-valid summaries
just to rebuild timing metadata from their source Facts.

The available-agent catalog also uses current Step instructions. DSH assembles
registered system-prompt sections for the current request; a native catalog
formerly appended the same replacement-marked instruction on every retry series,
but replacement affected compaction protection rather than immediate projection.
The existing BeforeStep instruction slot preserves current discovery, Human
mentions and source diagnostics without retaining every earlier catalog in model
history. Another registry or a last-good catalog cache would duplicate ownership
and could hide changed or removed definitions. The existing Step preparation
remains the durable retry/recovery receipt; this does not change spawn authority.

Unchanged Step instruction assemblies are omitted from subsequent preparations,
following DSH's `SystemPromptProjection.project` no-update decision. The current
Context cursor already retains the assembly for projection and checkpointing;
comparing that canonical value avoids a separate cache, digest table or storage
reference reader. Empty replacement is distinct from omission, and Step closure
clears the assembly. Each preparation still publishes its fresh application Tool
capture: those registration identities are not interchangeable merely because
the visible tool names and instruction text agree. The process adapter trusts
the executor's unchanged decision at this typed in-process boundary. Adding
another history walk to re-prove it would duplicate the owner's work.

The current Header format and Store schema reject older durable encodings. The
[Context contract](../../../../crates/rsi-agent/context/README.md) owns cache
versioning, including builder/catalog identity and immutable Header binding.
This pre-release choice avoids migration and fallback paths, not a claim that
migration is technically impossible.

Operation-scoped history reads borrow the captured claim-visible reader. Their
consumed cursor is extension checkpoint data and advances with the journal's
atomic publication. UTF-8 fragment transfer preserves the existing maximum Fact
size while keeping process frames small. It does not grant another Store reader
or permit access beyond the captured horizon.

Workbench history borrows a prepared page for the complete fragment transfer.
The [Serve contract](../../../../apps/serve/README.md) owns reader lifetimes and
bounds. Queueing before the first RPC leaves native request slots available to
finish or close existing transfers.

A draft's validated snapshot may reference unpublished CAS data. A proposal can
therefore retain a process-local owner for the complete snapshot. Baseline clones
and first-publication admission preserve it until canonical publication; replacing
a domain replaces its retained owner after validation. Retention does not expose
a Store writer or turn a projection into a state owner.

Commands have no originating Turn terminal fence. Their move-only publication
owner therefore retains a read-only outcome for reconciliation. Prewrite drop
proves refusal; entering Store admission disables that inference. Exact canonical
controls or applied draft state are recorded before notification. An unresolved
outcome permits positive canonical receipt lookup, never rollback from absence.
Command-produced text has explicit invocation provenance and is bound into the
same domain receipt. Draft text stays unpublished until the first mailbox submit.

Protocol 2 reserves admission for ping and close in both directions while keeping
one FIFO. Request identity remains monotonic across business and control traffic.
A slot retains its queued writes after a fast peer reply, so response speed cannot
bypass the bounded output queue. One deadline driver owns outstanding cancellation
acknowledgements; completed requests do not retain per-call timeout tasks.

## Alternatives considered

Native ABI polling would need another Rust queue, wake mechanism and cancellation
owner while preserving the callback gate's serial constraints. A second Node
database would split durable authority. Encoding journal metadata into the
model-visible Tool result would couple the provider contract to host bookkeeping.
Using Tool runtime `commit` as acknowledgement would confuse cleanup with durable
publication. Inferring rejection from an absent receipt before terminal admission
could race an already owned Kernel commit.

Rehashing every installed process dependency at startup makes activation cost
proportional to all shipped assets, including files that execution never reads.
The DSH boot profile resolves installed package manifests and module locations
instead. In the Windows fixture, restoring Better Sidebar spent about 64 seconds
acquiring a payload containing 13,079 files before the workbench could open.
Reusing only live leases cannot remove that cost after Host restart. More hashing
threads shift the same work onto scarce CPU and storage; timestamp caches add
state and invalidation rules. Metadata admission removes that work without an
additional cache or durable copy.

Stateless byte offsets for workbench history re-read and reserialize a complete
page for every fragment. This makes large-record work proportional to the number
of transport fragments. DSH's history controller borrows one Session observation
for each page computation; RSI's operation history and model streams likewise
already retain prepared data until consumed. A bounded active reader follows
that lifetime without retaining a second history cache or widening process frames.

Adding only an outbound heartbeat slot leaves the peer's inbound business limit
unchanged. Priority queues would reorder monotonic request identities. Reserving
both endpoints and accounting for queued writes preserves one framing path.
The old peer cannot uphold that contract, so the host, package requirement and
independent DSH peer use the new protocol together without a fallback handshake.

## Consequences

The process can overlap requests and reverse callbacks, but it is not a sandbox:
heap and CPU are not confined by the protocol's I/O bounds. A failed watchdog
terminates the generation, preserving effect uncertainty. Payload trees are
expanded under PackageStore's immutable object, lease and GC authority; large npm
closures do not use an independent mutable cache.
Process startup consequently does not proactively detect manual changes or
corruption in unused installed files. Installation still verifies every byte,
and ordinary acquisition remains the complete integrity check. This is an
intentional local execution trade-off: installed plugins already have the
operator's native authority, while bounded metadata, descriptor authentication,
no-follow path admission and GC leases remain enforced.
Workbench page transfer trades short-lived bounded memory for eliminating repeated
body reads, decode and serialization. Retention lasts only for the active transfer.
The standard package factory resolves a versioned descriptor from an exact
PackageStore digest. Descriptor paths can name the declaring package or its
exact direct dependencies; installation validates them before selection. The
external executable and complete environment remain operator configuration.
The connection's Process-settlement waiter retains the package closure even
after its last caller disappears. This avoids releasing files after merely
sending termination, before the process and pipes have settled.

These mechanisms do not by themselves establish full DSH compatibility. CLI
importer installation
and workbench service integration require their own owning contracts and tests.
The external DSH packages remain in their independent repository.

Native-sized process acceptance uses the product's multithread runtime shape.
A single-thread fixture can starve its own pipe and deadline tasks during large
serialization, reporting a heartbeat failure even while the peer replies promptly.
Matching the runtime preserves the actual watchdog contract instead of extending
its timeout to compensate for the fixture.

Fork transformation runs against a captured historical domain/Fact cut and
publishes only a child baseline. It never invokes business callbacks to recreate
state. Last windows need exact versioned projection seeds because refolding from
empty would lose Plan state preceding the selected conversation. Full follows
the source's inherited history: keeping ancestor messages only in plugin views
while omitting them from the native model context would expose conflicting
conversations. Context, maintenance and export therefore share that selection
semantics. Last remains a selection of completed local parent Turns; it does not
expand a window with earlier ancestor messages. This changes derived context
semantics and invalidates older Context caches, not the canonical Session format.

The workbench can populate its initial Session tree without acquiring a Kernel
or activating an Agent. Its request-scoped reader pages the caller-visible
Session catalog and reads the indexed durable activity summaries. The browser
workbench is a separate consumer of the Session owner. Making it depend on
the Agent adapter's in-memory registry would hide cold Sessions and would
start execution resources during navigation. The original Typert service and
codec can carry this read without a second browser transport.

Registry lookup and import have different lifetimes from the application.
Installing npm as an independent immutable object lets the application open
without acquiring a build toolchain. The importer retains the exact runtime
and npm dependencies needed to prepare its result. Explicit build approval
belongs to the original package manager's per-dependency policy. An approved
script runs with the local operator's OS authority in private staging before
PackageStore publishes its output.

A process lease protects files during execution. The profile's selected
packages must also survive a period with no running process, including when
another profile imports a different version under the same package name.
PackageStore owns exact per-profile GC roots under its existing writer lock.
Retaining old and candidate roots before replacing the portable view allows
restart to reconcile the Host journal after either publication boundary.
Importers invoked by an application reserve their candidate closures without
selecting packages for the CLI. The running application and its configured
importer retain their own bootstrap roots, independently of market changes.
