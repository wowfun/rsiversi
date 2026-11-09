# rsi-agent-store-protocol

Complete order membership includes immutable creation time and current activity
from the same Store snapshot. Shared Workspace account order uses creation time;
activity-based views retain the separate activity value. Neither requires loading
transcript bodies or a second timestamp record.

`session_summary` reads one durable Session's Header, activity timestamp and
Fact/control watermarks from one metadata snapshot. It returns the same
`StoreRecentSession` value used by recent listings, without loading transcript
or creating runtime activity. A missing or retired Session is not found.

A domain-state read can select one domain ID. It returns the same watermarks
and canonical state as filtering the complete page at that cut. A missing
domain yields an empty page. Readers can request the stored codec version by
name even when the active preset no longer registers it.

The mailbox names claim anchors as claimed Facts, independently of final
model input. Claim validation still compares the complete original accepted source
and content. References introduced by prepared replacements and additions retain
their ordinary media/CAS/Session edges. Local claim coordinates do not grant read
authority or add cross-Session retention edges.

`read_inbox_control_state` captures Fact/control watermarks, inbox suspension,
open-Turn and activation presence, unpaused waking-input presence and, when requested, all pending message identities in
acceptance order. A metadata-only read does not enumerate pending messages.
After ordinary Session validation, neither mode materializes message bodies,
Header JSON or the Agent tree. The Store
owns bounded identity decoding and completeness at that one snapshot; Kernel
uses this metadata to prepare a Session stop and observe driver quiescence under
submission admission. Activation and waking presence use existing indexes;
quiescence reads do not enumerate queued identities.

Inbox pause state is a mechanical projection of `InboxPaused` and
`InboxResumed`, initially false. Ready roots/messages and subtree waking status
exclude a paused Session without deleting its candidate messages. Mailbox and
inspection snapshots expose this state at the same durable cut as their
contents. Promotion and queue editing preserve it. The Store does not decide
which user or Agent action requests a pause or resume; Kernel owns that policy.

`read_agent_mailbox_summary` also returns the source kind of the first pending
next-Step message in acceptance order, or none when that selection is empty.
It shares the summary's Fact/control cut and uses indexed metadata after ordinary
Session validation. Observing whether an Agent wait can resume does not load
message bodies or consume the queue; Kernel maps the source kind to the wake cause.

Each control record retains the immutable Fact watermark at the end of its
own atomic append. `StoreControlPage.fact_cursors` pairs those watermarks with
the returned records, and `durable_fact_seq` comes from the same read snapshot.
Anchors are nondecreasing in control order and cannot exceed the Fact tail.
For a single observed event sequence, Facts in one append precede that append's
controls. Controls sharing an anchor keep their control order. This is a
commit-order relation, independent of timestamps and caller timing; it adds no
control message to the model's conversation context.

Fork boundary selection accepts a typed point before an invoking Turn or through
an already terminal Turn. Both modes resolve the same balanced completed-Turn
window and exact Fact/control digests in one read snapshot. An open through-Turn
is rejected; selection never advances to a later terminal. Historical references
and Agent-tree membership remain separate Store edges.

Command input is part of the canonical domain request digest. Every bound message
must appear immediately after that domain control in the same atomic append, with
identical identity, source, content and delivery intent. Extra unbound command
messages, missing messages and reordered or substituted payloads are rejected.

Reference maintenance recognizes the built-in `ExtensionJournal` envelope
independently of plugin registration. It validates that exact version and visits
only explicit typed references, so uninstalling an extension does not lose its
Agent-CAS roots. Other domain identities retain exact registered-codec semantics.

Product purge previews distinguish `UncertainWork` (missing native settlement
evidence) from `Busy` (work that can still settle). Such a blocker does not admit
deletion or imply that retrying the mutation can resolve the native owner.

Explicit Session maintenance uses a sorted bounded selection with immutable Header
fingerprints and both durable cursors. It never expands to descendants or cancels
execution. The product must close its live owners before admitting retirement;
Store independently rechecks durable idleness, complete reference coverage and
incoming edges outside the explicit selection during initial admission.
An operation identity retains its original request and progress across restart.
Purge recovery retains only each selected execution location for product
authorization, not copies of the deleted Headers, configuration or workspace paths.
The immutable selection and tombstones remain necessary to reject identity reuse
and correlate repeated operations. Purge is logical deletion, not secure erasure
of SQLite free pages, WAL files or backups.
Persistent retirement tombstones survive Header deletion and reject resurrection.
Each deletion transaction admits at most 512 logical records and 64 MiB + 384 bytes
of stored payload. The extra 384 bytes accommodate a maximum-size Context checkpoint's
256-byte Session identity and two 64-byte digests. This excludes WAL and index overhead;
writer ownership is released between
batches. Session purge retains Agent CAS and Media objects. Object collection and
exclusive database compaction are separate operations with separate receipts.

Canonical reference indexes derive only from decoded Headers, controls, Facts and
exact pure Domain codecs. Missing codecs return Unsupported. Missing, extra or
changed index edges are corruption, never proof of absence. Maintenance enables
strict admission only after a successful coverage audit and an atomic recheck for
concurrent incomplete coverage; a failed audit does not change write policy. New
writes then maintain coverage and edges in their canonical transaction and cannot
reference a retiring Session or deleting CAS object.

Model intent, start and settlement coordinates are unique by Session/Turn/effect/role
across the complete stored history. Append and atomic Agent commit enforce this
mechanical uniqueness in the same transaction as the Facts. Kernel validates
lifecycle ordering and exact Turn/purpose; it does not retain a set of every
completed model identity in a live Turn. A durable intent existence query supplies
the historical input for Kernel admission; providers may implement it through
bounded pages or their canonical-Fact index. Reusing an effect in a different
Turn is valid. A new intent must commit before dispatch.

`StoreError::Unsupported` identifies a missing provider or platform capability
before Store admission. It is distinct from malformed caller input (`Invalid`)
and from an uncertain durable write outcome.

Activity enumeration is a metadata-only live keyset over `(last_activity_ms,
SessionId)` descending, globally or within exact execution coordinates. It reads
neither transcript nor complete Headers and does not mount a Session. Each page
and its newest cursor share one read snapshot. Each activity row includes its
Session's durable Fact watermark from that snapshot. Activity commits atomically with
the owning append and never regresses under wall-clock rollback. Header creation,
new human MessageAccepted controls (excluding same-commit queue successors),
TurnAccepted, ImageRequested, CancelRequested, ImageOutput, ToolResult,
ToolRejected and Conversation model settlements count. Every settlement contains
its qualifying Finished or Failed terminal event, including tool-only and
reasoning-only attempts; Context-compaction settlements do not count.
Other control and Fact kinds do not count.

Access batches are a required Store operation: 1..=256 unique Session identities
return ordered per-item location/protection results from one metadata snapshot.
Activity-cut batches likewise require 1..=64 unique identities and return each
Session's durable Fact tail and whether an open Turn exists. Successful cuts in
one batch share one read snapshot; separate batches need not share a horizon.
Providers establish canonical-history validation before reading Turn indexes.
Missing, retired or corrupt Sessions are per-item errors; failure to admit or
read the batch is an outer error. Neither operation grants product authority.
Invalid selection diagnostics identify the access or activity-cut operation and
its cardinality and uniqueness requirements.

Complete ordering membership is a separate single-snapshot query returning at
most 1024 identities within 128 KiB of encoded JSON. Overflow returns an explicit
TooLarge result, never a truncated list masquerading as complete membership.
These indexes are projections of durable coordinates and records, not execution
or machine-use authority.
Creation-ordered recent pages, activity pages and complete membership accept an explicit bounded location
selection. Providers apply it inside the read snapshot, before row/byte limits
and newest-cursor selection. Unauthorized locations cannot consume a caller's
membership budget or appear as continuation metadata. Store treats the selector
as mechanical query data; its caller must retain the corresponding admission.

Exact message reads return only the selected indexed mailbox entry, including its
terminal state. They validate it against the durable control tail in the same
snapshot and never materialize or count unrelated pending message bodies.

Finite suffix capture returns a read-time Fact horizon and prefix digest from
the same metadata snapshot as its bounded backward page. The caller supplies
count and encoded-byte limits; providers check lengths before copying bodies,
including the first body. An oversized last Fact yields an empty omitted suffix.
This mechanical operation performs no conversation export or reference policy.

Finite forward windows likewise admit encoded lengths before copying any body.
They inspect at most 256 contiguous coordinates and retain at most the caller's
byte budget, including the first record. A record larger than the complete budget
is returned as an explicit length-only omission so a derived index can advance;
a record that merely exceeds the remaining budget is left for the next window.
The read-time durable horizon, returned interval, admitted bodies and omissions
come from one snapshot. This operation supplies no search or reference authority.

Evidence originals are resolved by exact Session and Fact sequence. The shared
resolver accepts only a ModelIntent's original inline section with matching
section, digest and byte length; reference chains are rejected. Consumers may
cache the resulting digest descriptors within the owning Store lifetime, bounded
independently of history size. Descriptors retain no evidence text. Fact decoding
still validates the original bytes at the durable boundary.
Consumers retaining one selected source can transfer its owned String after
the same binding check, releasing the other sections without copying that text.

Stores implement only the current `AGENT_STORE_SCHEMA_VERSION` defined in
[src/lib.rs](src/lib.rs). The [SQLite contract](../store-sqlite/README.md)
owns database preflight and file-preservation guarantees.

## Domain mutations

Domain state is canonical only in `DomainStateCommitted` controls. One record
contains all distinct-domain replacements for a bounded request, its expected
revisions and Kernel-assigned source. A receipt identifies that control and the
request digest. Domain head/as-of and request indexes retain control positions,
never a second authoritative state payload. Store does not invoke domain codecs.
For command controls, the retained invocation's expected control revision must
be the exact predecessor of that canonical control, in addition to each domain
replacement's revision check.

A mixed Turn mutation binds its exact contiguous same-append Fact span and a
digest of those Fact bodies. The request digest includes that body digest,
but excludes allocated sequences and timestamps. Store admission validates the
binding before either stream changes; cold validation checks the canonical
Fact interval again. A retry cannot change Facts while retaining the same
domain request identity. Baselines and external commands do not claim Turn Facts.

The initial nonempty baseline is control one of the atomic fresh Header and
first acceptance commit; an omitted baseline denotes an empty domain set.
Only baseline admission creates a domain. Later mutations require an existing
matching codec and exact predecessor revision. Same request identity with
different content conflicts. Querying a committed request recovers the exact
receipt after an unknown result. Selecting a historical state uses the paired
terminal control horizon, so later idle mutations cannot enter that fork.
Domain read consumers bind the returned selected horizon to their request:
an explicit cursor must match exactly, and a current-state read must select the
reported durable control tail. Structural page validation alone cannot distinguish
a valid historical page returned in place of current state.

A reader of an authorized Session's inherited history captures its parent
intervals in replay order. Each interval holds a Store validation lease and
matches its recorded parent Header fingerprint, effective turns, Fact boundary
and control boundary. An `all` interval follows the parent's own inheritance.
Other selections stop the walk at that parent.

## Correlated terminal commits

Every terminal Fact is committed with one `TurnBoundaryRecorded` control in the
same `AtomicAgentCommit`. Each Session append contains at most one terminal;
its marker is the final control, names that exact Turn and Fact sequence, and
defines the terminal's control horizon. A marker without that same-append
terminal, a duplicate, a later business control, or a Fact-only terminal append
is rejected before mutation. Recovery repairs separate terminal boundaries in
separate appends. This permits one exact Fact/control prefix pair for every
completed historical Turn without inferring a control horizon from timestamps.

The terminal index retains both sequences and rolling prefix digests, derived
only from canonical records. Fork boundary reads return that exact pair.
`none` inherits neither prefix; `all` and `N` use the selected terminal's control
horizon, so later idle commands cannot alter an already selected historical
cut. Index and offline validation reject missing, mismatched or fabricated
terminal correlations. The schema cutover is explicit; old database files are
preserved and never automatically rewritten.

`inspect_session` is one bounded read snapshot of immutable Header, durable
Fact/control cursors, pending-message metadata, current Turn and activation
phase, and complete bounded descendant activity. It decodes no message bodies
or historical Facts. The Memory adapter holds one state lock; SQLite uses one
read transaction. Clients cannot reconstruct this contract by combining
independent reads across concurrent commits.

Mailbox entries retain immutable delivery intent, original acceptance timestamp,
and an optional bound steering Turn separately from their mutable current
target. Pending next-Step completion and bound human-steer messages can be
promoted at terminal settlement. Human-steer ready keys retain their acceptance
timestamp/control sequence; Completion ready keys retain promotion order.
Both adapters validate this against the canonical control stream. A bound steer
requires the matching current activation in Running or Parked phase. Duplicate
queue operation IDs are invalid commits, not transient I/O failures; rejection
leaves both streams and all derived indexes unchanged.

Ready-message pages carry the authority-neutral source kind without message
bodies. The source kind comes from the same classifier as the mailbox index;
it does not replace canonical Session validation or continuation admission.
Missing mailbox rows and inconsistent ready routing remain corruption, including
the pagination lookahead row. Ordinary ready selection does not materialize a
pending mailbox merely to classify its selected message.

With the Session validation proof warm, fork `none` does not query completed
history, and fixed `Last(k)` selection visits only the bounded completed suffix.
Invoking-Turn identity, terminal digests and the selected Fact interval are still
validated. Cold canonical validation and work proportional to selected history
are outside that selection-work bound.

This crate owns the mechanical durable seam for Agent sessions. A Store accepts
one immutable header, contiguous compare-and-append Fact batches, bounded
reads, session enumeration for recovery, and immutable CAS objects.
Append and atomic-commit suffixes own `Arc<SessionFact>` handles. Transferring
or retrying a prepared suffix shares immutable payloads; adapters validate and
encode borrowed Facts. The Memory adapter retains those same allocations.
Read pages keep their separate bounded materialization contract.
Alongside
the canonical session sequence it transactionally maintains mechanical turn
membership and open/terminal indexes. Those indexes select durable bytes; they
do not apply effect transitions, classify recovery, or select a turn outcome.
The protocol-owned `store_fact_turn_role` classifier is the single authority
for acceptance, terminal, and in-turn event membership used by Store adapters.
One exact turn-boundary read validates and returns the indexed acceptance
sequence, optional typed terminal Fact, and read-time durable watermark without
materializing intervening event Facts. Each returned Fact must match the exact
session sequence and relational turn/kind selected by its index; typed JSON
validity alone is not an index proof. Kernel alone interprets the terminal outcome.
See [the Agent architecture](../docs/architecture.md).

`header` and recent-session listing validate bounded immutable metadata only;
they do not scan history or establish a historical validation proof.
`validate_session` explicitly checks the mechanical durable session invariants.
Execution, recovery, lineage, and control decisions require that proof before
using a cold Header. Callers retain `prepare_session` across canonical reads
used for these decisions. Turn, outcome, mailbox, domain, lineage, checkpoint,
and mutation boundaries establish the proof themselves.

Canonical Fact pages (forward, backward, window and suffix) and control pages
validate only their selected interval in one read snapshot: framing and byte
bounds, typed records, physical/logical coordinate agreement, contiguous progress,
and read-time watermarks. Control anchors remain nondecreasing across the
preceding cursor and within the page. These reads neither inspect unrelated history or derived indexes nor
establish a Session proof. A malformed selected record fails; unrelated dormant
corruption is left to explicit validation, indexed decisions and offline audit.
The suffix digest is read-time metadata, not a recomputed prefix proof.
A backend may cache successful proofs within its exclusive writer lifetime;
metadata and canonical page reads neither fill nor touch that cache.

Agent control commits compare both Fact and control watermarks and atomically
touch at most three sessions. A `MessageClaimed` control supplies its input Fact
from the canonical accepted message when the caller does not supply that Fact.
The Kernel chooses the message, Turn, Step, timestamp and exact Fact sequence;
Store only reconstructs the body and validates its pending state. Explicit Facts
and these references must form one contiguous suffix, with a unique claim per
message and Fact. The batch byte budget counts supplied payloads and references,
not already-durable bodies. SQLite resolves and hashes one accepted body at a
time inside the same transaction; it never collects the expanded claim batch.
Logical Fact readers retain their existing byte bounds and representation.

`read_mailbox_claim` captures the complete selected identity list, mailbox flags,
Fact/control tails and optional exact waking message in one snapshot. Without a
waking identity it selects next-Step messages; with one it also selects that
message and same-command siblings. This metadata selection has the mailbox's
cardinality bound, independently of the payload page bound. Kernel orders all
selected next-Step inputs before waking inputs and claims them atomically.

Within each Session append, capacity and reservation
checks observe controls in sequence order, including slots released or consumed
earlier in that append. Program graph counterparts are checked after all paired
appends are installed. The Store maintains mechanical indexes with
bounded query and atomic-update surfaces for ready messages, immutable
parent-child lineage, terminal-prefix digests,
a byte-bounded prefix plus the exact pending-mailbox count and direct message
status, and the one currently active activation per session. A lightweight
mailbox summary returns the pending count, the bounded ordered identities of
pending promotable next-Step messages, and Fact/control tails for capacity and
terminal-promotion decisions. Payload inspection reads all pending messages,
optionally with one exact message status. Claim metadata selection separately
reports whether any ordinary next-Turn input is pending, so continuation
priority needs no unrelated message bodies. Every read uses one Store snapshot:
it never materializes the valid 64-message worst case at once, returns the Fact
and control tails observed by that snapshot, and lets callers reserve its fixed
page bound before I/O. Message admission
and status reads use that index rather than replaying an unbounded control
history. Each returned entry carries the exact encoded message length computed
by the Store while it reads or indexes the payload. The mailbox page takes at
most half the atomic batch byte budget. This payload inspection page does not
limit the metadata selection used for claiming. A descendant
status snapshot reads one subtree's immutable membership, parent/path/task,
control watermarks, the last settlement control sequence, and open-Turn,
active-activation and waking-message flags
in one Store snapshot; Agent waits use it as their race-free
observation baseline. The settlement sequence is zero when absent and cannot
exceed the captured control tail. Backends maintain it atomically with appends;
the Kernel interprets an advancement inside its captured interval as completion.
Activation guards check exact ownership before applying
appends; quiescence guards check the resulting indexed state before committing.
The Kernel chooses policy and the Store proves these atomic conditions. Guard failures have
dedicated errors and are not encoded as synthetic Fact-sequence conflicts. This
prevents a parent settlement from racing a child claim.
Quiescence guards name a subtree root, not a previously enumerated list. Within
the same write transaction, after all appends, the Store checks every strict
descendant against all three busy indexes. This includes children introduced by
that transaction. The root may itself be newly created and is excluded.
Traversal rejects cycles and trees exceeding 256 nodes.
An append without a Header requires an existing Session. Its absence is
reported as `NotFound` before Fact or control cursor conflicts, identically in
the production backend and the shared in-memory contract fixture.
An active-activation lookup likewise returns `NotFound` for an absent Session;
`None` denotes an existing Session with no active activation.

The lineage index is derived from immutable Headers rather than trusting
duplicated root labels in writes. A child root must equal its parent's derived
root, and the Kernel derives every accepted mailbox message root from the
target's prepared Header before it can enter either mailbox or ready indexes.
The Store revalidates that derived root. Memory, SQLite, and offline verification
enforce the same rule.

Ordinary forward/backward Fact pages, per-turn Fact pages and control pages accept
an explicit count (1..512) and encoded-byte budget (1..64 MiB). The same snapshot
checks each stored length before copying or decoding its body. If the first
valid record exceeds the complete budget, ReadRequiresCapacity reports its exact
minimum_bytes with no body materialized. Later records that exceed the remaining
budget end a continuous short page; callers continue by cursor/has_more, never by
comparing the returned count with the requested count. ReadCapacity remains a
separate allocation-admission refusal. Oversized durable records remain Corrupt.
No separate size probe or durable length column is part of this contract.
`admit_page_record` owns the shared first-record refusal versus short-page rule
for implementations that have already validated their page limits and lengths.

Each open-turn page includes the durable Fact and control tails from the same snapshot.
Recovery can use both as commit preconditions without fetching a control record.
Indexed Session activity summaries also carry both durable watermarks. Consumers
can identify a changed log from a single batch of metadata reads.

Forward and backward Session Fact reads, per-turn Fact reads, open-turn
enumeration, lexical session enumeration, lexical enumeration restricted to
sessions with open turns, and creation-time-ordered recent-session enumeration
are cursor-paginated with protocol-owned count and byte limits. Backward reads
take an exclusive sequence cursor and still return Facts in ascending sequence
order; a nonempty page ends at `before_seq - 1`, while only cursor one may
produce an empty page. The recent-session cursor is the exact descending
`(created_at_ms, session_id)` key; lexical enumeration remains the recovery
contract and is not reused for presentation. Startup recovery uses the
open-session index, so closed historical sessions do not impose Store calls or
decoding work. No Store method materializes an unbounded durable catalog.
Each recent-session row carries its validated bounded Header, last qualifying
activity time and both durable watermarks from the same Store read snapshot.
Providers validate these rows at the Store boundary. Consumers can render a
catalog or compare log revisions without later Header or activity-summary reads.
The row contains no duplicate identity or execution coordinates beside its Header.

Context checkpoints are an optional cache, not canonical session state. The
Store preserves at most 64 MiB of opaque Context-owned bytes with a header
fingerprint, cursor, and lowercase SHA-256 digest of the folded Fact prefix, and
installs them only when that cursor exactly equals the durable tail and both
fingerprint and prefix digest equal Store-owned values derived from the
canonical header and Fact log. A missing, stale, corrupt, or unsupported
checkpoint never changes Fact replay semantics.

The Store also derives the latest instruction-baseline and skill-catalog
SHA-256 values from canonical input Facts. This bounded projection lets a cold
Kernel restore replacement suppression without replaying a whole closed
history; it is mechanical cache state, and offline verification recomputes it
from the Fact stream.

Activation starts must match the immutable Header's root, parent, and Agent
path. Store adapters enforce this relationship before committing any control
or index row; SQLite verification independently rechecks historical starts.

Inspection's pending-message metadata carries the immutable promotion capability
(Completion source or steering intent bound to a Turn), so a promoted Completion remains a valid
next-Turn route without materializing its payload.

## Prepared reads and scalar watermarks

`prepare_session` returns an opaque `SessionValidationLease` that keeps the
selected Session's successful mechanical validation available through ordinary
cache eviction. It grants no execution authority or snapshot isolation. Trusted
Store mutations preserve that proof within the same Store ownership lifetime.
`read_watermarks` reads one simultaneous pair of durable Fact/control cursors
without validating history or decoding payloads. Consumers use those cursors for
CAS; commits and indexed history operations retain their validation obligations.
`StoreSessionWatermarks` represents both a read snapshot and committed cursors.

ProgramRun controls retain exact per-run descriptor, lifecycle, initial-child
receipts and cumulative run-record byte accounting. Stores index these controls
mechanically and compare their canonical replay during validation; they do not
mint live authority or schedule scripts. Paired parent-run and child mailbox
appends atomically admit and settle children. A terminal run cannot admit another
child. Ordinary parent-mailbox reservation accounting excludes run-owned receipts.
Execution-owned descendant quiescence is distinct from complete tree idleness;
both guards validate membership and activity in the committing transaction.
Complete-idleness guards include active Programs in the root and descendants;
activation-owned guards exclude the root Program and independently owned branches.

Program record reads may select only controls strictly after a retained cursor.
Each atomic snapshot returns the current mechanical head and all later indexed
records, bounded by the run's 8 MiB / 4096-record limit. Consumers validate the
delta against the previously validated head; an empty delta must preserve it.

Program-owned child Headers require atomic Agent commits with their paired parent admission and initial input; ordinary Fact-only creation cannot express this graph.
Program graph validation runs after every affected atomic append, so paired parent
and child controls may appear in either write order. It proves child Headers and
initial messages against parent admissions (new admissions must leave a pending,
waking NextTurn input), initial claims against exclusive
Program completion sinks, child receipts against the exact discard or activation
outcome and complete structured-result binding,
and notice coordinates against one matching terminal run. The same mechanical
checks run during SQLite verification; they do not decide execution policy.

`program_run_for_creator` uses the unique Session/creator-Turn index to preflight admission before CAS publication. A second Program for that Turn remains forbidden after the first terminalizes and is rejected as `StoreError::Invalid` without changing durable cursors.

SQLite indexes Program activation counterparts by Session, control kind and exact
activation identity. Proof lookup does not scan unrelated activation history.

## Queue successors and receipts

A queue slot starts with the initial MessageId and survives Human replacement or
conversion. Each successor has a new MessageId, acceptance timestamp and control
sequence. Append-only successor controls retain the predecessor and inherit its
original ready-order key: root grouping, timestamp, SessionId and ready control
sequence. Neither original payload nor delivery intent is rewritten, including
prefixes already pinned by a fork. Steer promotion preserves that same order.
Withdrawal permits every pending source and never changes the original source.
Kernel refuses ordinary withdrawal of a ProgramRun-owned child's initial input;
its cancellation commits the discard and parent settlement together. Store's
Program graph validation checks that settlement against the discarded child input.

Store admission discards the predecessor and its ready row before accepting the
successor in one transaction. A full 64-message mailbox can therefore replace an
entry without temporary capacity overflow; failure rolls the entire transaction
back. Store graph validation independently rejects a successor whose
predecessor is no longer pending or belongs to another slot.

Each admitted queue operation retains a receipt of at most 4 KiB, indexed by
operation identity into the canonical control stream until Session deletion.
There is no lifetime receipt count limit. Both success and rejection must remain
replayable to distinguish an unknown outcome from a new request. Disk history may
grow with use; indexed reads and bounded append batches do not load that history
into memory. SQLite's offline verifier checks derived indices against canonical
controls. Receipt counts are an inspection/validation operation, not admission.

`agent_message_exists` checks an indexed identity without loading message content.
It uses the Store's validated Session boundary; callers reserve only metadata
capacity for this probe. Full payload reads retain their separate byte budget.

`message_permits_promotion` owns the shared ingress promotion rule used by Store
validation and client projections. Source kind, resolved delivery and an exact
Turn binding determine the result; adapters do not duplicate that decision.

Complete order seeds dictionary-encode execution coordinates and carry a group
index and indexed last-activity key beside every SessionId. Both derive from the same read transaction, so a
flat view can reject cross-machine/workspace moves without reading all summaries.
Groups are unique, referenced, in first-member order, and share the seed's 128 KiB
bound. Summary pages are independent later read snapshots.

Program history selects at most sixteen canonical mechanical heads in descending
acceptance-control order within one Session and a fixed watermark. Store owns the
index and checks its acceptance projection; Kernel validates page count, descending
order, cursor membership and progress before allocating views, and owns lifecycle folding. CAS
reads accept ByteAdmission, reserve in the actual owning worker before payload
allocation, and return RetainedBytes whose shared lease survives the last clone. Missing metadata or a missing
CAS file returns `NotFound`; malformed files and inconsistent metadata remain
`Corrupt`, and other filesystem faults remain `Io`.

CAS publication returns a non-serializable `PreparedCasObject`. Its retained lease
blocks reclamation until the canonical referencing commit finishes. A new lease
for an existing descriptor checks its registry identity, length and deletion fence
without reading the payload; it never recreates a missing registry entry. It is
ownership, not a byte-integrity proof. Consumers must authenticate newly loaded
bytes with `read_cas`; a previously verified immutable cache can reuse its bytes. Active readers retain the same ownership through actual I/O,
including cancelled waiters. Agent CAS collection is explicit, checks complete
typed reference coverage, and admits one hash/generation under exclusive local
ownership plus a durable deleting receipt. Canonical appends reject deleting or
missing objects. Namespace removal and directory sync precede metadata removal;
completed old receipts cannot remove a later publication of the same hash.

Native canonical reference projection is shared by the derived edge index and
offline CAS inspection. Domain state is added only through exact registered codecs;
native-only inspection does not infer plugin references.

Pending maintenance can be rediscovered through `StoreMaintenance` without a
remembered operation ID. Purge and CAS collection each expose a read-only,
lexically ordered page of at most 256 unfinished IDs, with an exclusive optional
after-ID cursor. A short page ends the current scan; concurrent completions may
remove IDs. Discovery grants no mutation authority and never resumes work. Use the
existing binding/status methods and ordinary authorization before explicit retry.

Session access batches select 1..256 unique identities in caller order from one
metadata snapshot. Each result retains only SessionId, execution location and
optional protection scope. Providers validate each full bounded Header before
projection and release it before the next row. Per-item NotFound/Corrupt errors
remain ordered; a batch failure describes the whole snapshot. This seam grants
no access and does not validate historical Facts.
