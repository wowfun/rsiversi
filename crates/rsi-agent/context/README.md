# rsi-agent-context

Target-specific Tool image fallback consumes the assembled messages before request
construction, moving unaffected messages and request options without cloning. Only Tool results containing
images are rebuilt; complete-request validation still bounds the projection.
The allocation golden deliberately binds builder 2.8.0 and Session format 19,
while retaining the v10 fold and v6 envelope formats.

The single deep module for prompt projection, incremental model context, and
deterministic compaction. It consumes validated session Facts and emits bounded
provider-neutral Language messages. It never reads a Workspace implicitly and
never stores a second transcript.

Semantic compaction is a pure planning and replay operation over model-purpose
Facts. The selected builder's complete identity, Header, source Turn/Fact spans
and digests, prior summary coverage, Usage horizon and canonical view digest
bind a frozen plan. The executor supplies provider events; only natural Stop
with nonempty visible text, no Tool calls, at most 32 KiB of text and a strictly
smaller canonical view installs the summary. Reasoning is not summary content.
Invalid or mismatched summaries are inert, including summaries whose transitive
source is outside the exact fork selection. Raw Facts remain authoritative.

Pressure uses the most recent finished Conversation's reported input tokens
for the same ModelRef, at eighty percent of described context window minus
default output reserve. Summary Usage and Usage covered by an installed summary
are ineligible. Installing an eligible summary consumes the preceding Usage,
including inherited parent Usage; only new Conversation Usage can rearm it.
An optional Usage trigger with no selectable whole unit leaves the ordinary
request unchanged. Forced or canonical pressure without a selection is a limit.
No estimate substitutes for missing Usage. Hard canonical
byte/message limits and explicit provider ContextLimit handle first calls.
Compaction preserves active instructions, the current original task input, latest
human steering and whole recent interaction units. The optional recent tail
uses at most 64 KiB and half the lesser of the configured and AI message
allowances; the last unit is always retained intact. A terminal Turn's incomplete
Tool batch and a durably superseded live batch remain protected; unrelated
complete units remain eligible. Original unit coordinates never include derived
results. Other unfinished live batches and orphan results are invalid.
The protected incomplete units still count toward the 4,096-message / 32 MiB
materialization ceiling. Repeated interrupted or superseded batches can exhaust
that irreducible history; compaction then returns `TooLarge` rather than deleting
unknown-outcome evidence. Summarizing such units requires a separate change to
the raw-coordinate selection and replay contract.
Call identifiers are unique within a retained Turn. Outcome admission rejects duplicate,
unknown, already superseded or mismatched calls before changing their state.
The AI protocol separately requires call identifiers to be unique across the
complete emitted request. A provider reusing an identifier across retained Turns
is rejected at request construction; the fold does not rename durable call IDs.
Checkpoint restore also rejects orphan results, missing settled results, and
result order inconsistent with the call batch. Settlement rejects a result that
would follow a later call's result or leave its batch nonadjacent. A successful
parallel sibling may still settle when an earlier sibling has no outcome; after
termination its missing predecessor receives the ordinary unknown-outcome view.
Message capacity rejection cannot register a batch or settle a call without its
message. An error after Fact workspace admission invalidates the cursor for
further use: mutable projection, semantic state and assemblers are dropped,
temporary credit is released, and only the immutable Header remains charged.
Sequence and prefix-digest metadata retain the last successful prefix for diagnostics.
Further ingestion, projection, planning and checkpoint creation reject that
cursor; discard it and rebuild from authoritative Facts. Sequence/Fact-validation
rejections and workspace admission refusals do not mutate the cursor. A failed
Fact body cannot yield a cache claiming to represent the preceding prefix.

Provider requests retain actual results and add deterministic error explanations
for missing results: superseded before execution, terminal before ToolStarted,
or started with no durable outcome. These are views, never ToolResult Facts or
permission to replay a Tool. Ordinary requests, pressure, view digests and
summary shrink checks share that view. Non-semantic requests also budget the
normalized view before deciding which completed turns to omit, so synthesized
results cannot invalidate a raw-only retention decision. Their byte accounting
reuses retained per-Turn totals and serializes only synthesized outcomes.
Raw Facts remain unchanged.

Retention and emission are distinct. Each attempt supplies its described target
LanguageProfile along with frozen Language request options. Builder 2.8.0 emits
nested ToolResult images only when that profile positively declares image Tool
results. No or Unknown uses deterministic descriptor text in the same block
position, without reading Media or changing call IDs and error status. Ordinary
user images retain the AI provider's validation contract. The rich fold and
checkpoints retain image descriptors, so a later vision request can consume them.
This final request projection does not change compaction sources, selection or
digests. Its complete message and request budgets are checked after projection.

Each attempt freezes validated Language
request options before planning. The effective message budget intersects the
configured limits with the AI message ceiling and the exact bytes remaining in
the complete request. System, summary and interruption explanations count.
Dynamic options trigger pressure but do not change replayable selection rules.
Before summary I/O, the irreducible protected view plus a minimal summary must
fit. This is a feasibility lower bound, not a guarantee that one bounded summary
will make an ordinary request fit. Source-count, quoted-byte and retry bounds can
require multiple successful plans; each installed plan must strictly shrink the
canonical view and remains replayable independently of current emission pressure.

A capacity retry
halves selected canonical message bytes, choosing whole units oldest-first and
skipping any unit that cannot fit the remaining allowance.
An instruction replacement supersedes earlier instructions from that same
source, including tombstones; a complete skill catalog supersedes its preceding
catalog. Those historical versions become ordinary summary candidates. Additive
instructions stay protected until an explicit replacement of their source;
instructions from other sources retain their protection.

Builder 2.7.0 prunes historical Tool-result text at view time. The fold and its
checkpoint retain the original projected messages. Above 8,192 Unicode code
points, the view keeps the first 4,096 and last 1,024, separated by
`\n\n[... tool result middle pruned ...]\n\n`. Text budgets span all text blocks
of one result; non-text blocks retain their order. The JSON fallback used when a
Tool has no content is also text and may cease to be complete JSON after pruning.
The latest complete interaction unit and every incomplete unit remain intact.
An ordered partial prefix is readable; orphan or misordered results are invalid.
Summary planning still rejects an unfinished live interaction.

Ordinary requests, summary input and selection budgets, view digests, replay
eligibility and post-install shrink checks use the same pure projection. One planning
call reuses a single projected history for pressure, selection and materialization.
Forced, message-count and source-count pressure skip the separate byte-trigger
pass; no-pressure attempts do not hash the view. Live outcome validation traverses
without materializing messages. Remainder preflight and installation use the same
post-selection pruning and provider-view assembly.
Remainder measurement borrows turns with no removed messages; turns changed by
selection own their retained subset. Selection byte counting borrows
provider-neutral messages and copies only provider-state changes.
Unchanged turns borrow their retained messages; only turns containing a pruned
result are copied. Replay eligibility also shares one projection across its checks.
Planning and replay hash the borrowed semantic view directly into a counting
SHA-256 writer, without copying unchanged messages or buffering their JSON.
Provider-private reasoning is removed under the same projection rules as ordinary
requests; the view bytes and durable digests remain identical.
Stable message coordinates and raw Fact/source digests do not change. Equal identity,
prefix and position produce equal views; a child with additional input need not
match its parent's former view. Pruning occurs on every view, independently of
the existing reported-Usage summary trigger. It does not relax raw materialization
bounds. Older builder summaries and caches are ineligible under the new identity;
fallback to raw Facts can reach those bounds.

The builder renders frozen reference previews as user data with exact recorded
read coordinates. It performs no CAS reads. It also binds newly selected source spans plus the exact previously
installed summary. That prior is an inductive proof: it is usable only after
its own sources and prior were validated in this same replay/fork selection.
A source binding describes the frozen Fact prefix at summary intent, not the
latest source head at installation. Eligibility validates that complete prefix
before the intent is recorded; subsequent summary Facts advance the source head
without altering its bound prefix. Installation rechecks the projected view and
shrink, while replay repeats the intent-time validation. Comparing the frozen
binding to the latest head would reject valid summaries of an active Turn.
Transitive raw bindings are not copied into every descendant plan. Fully
summarized completed Turns and their source metadata are released. The cold
materialization bounds count projected messages (4,096 / 32 MiB), not Facts.
Both fold modes admit messages against these absolute limits before retention,
including when an active oldest Turn prevents eviction. Incremental non-semantic
folds may first evict complete oldest Turns; active Turns are never truncated.
Raw source metadata shares the 4,096 cold retention ceiling. At 1,024 retained
sources, every subsequent planning opportunity requests compaction. Each plan
selects at most 1,024 source Turns, oldest first. Selection stops before the
encoded sources and selections exceed 240 KiB, reserving 16 KiB within the
protocol's 256 KiB plan bound for bounded identity, horizon and summary metadata.
This preserves whole interaction units even with maximum-length identifiers;
the same rule governs replay eligibility and the smaller retry.
Selection also counts the JSON-quoted source bytes, prior summary and complete
no-Tool request envelope against the AI protocol's request-byte bound. Units
that cannot fit the remaining request allowance are skipped intact. A single
unsplittable unit above that allowance stays raw; forced pressure can still fail.
Skipping that first opportunity
does not invalidate later replay within the cold bounds; raw history is never
silently evicted to make room.

Entered plugin context is durable text with developer role. A pre-start Tool
rejection becomes an error response for its exact model call, without inventing
Tool execution or consulting a current plugin during replay.

`ModelContextBuilder` is a synchronous, process-local Local capability. It opens
one mutable `ModelContextCursor` from a validated immutable Header, retention
limits, and an optional bounded provider checkpoint payload. Cursors consume
framework-supplied canonical pages, claim-visible pages with their scan horizon,
fork seed pages, and explicit seed completion as distinct inputs. They build
provider-neutral requests from the Tool definitions supplied by the same Agent
composition pin. Builders and cursors perform no external I/O or implicit clock
sampling. The ordinary `DefaultContextBuilderFactory` provides semantic
compaction over ContextFold and accepts only null configuration; selecting it is an
explicit Agent Profile choice.

`ModelContextState` owns the selected builder, cursor, and version-6 cache
envelope. The envelope binds the builder ID, semantic version and normalized
configuration digest, Header fingerprint, exact limits, cursor and Fact prefix
to the raw bounded provider payload. Restore validates this envelope before
calling the builder and requires the restored cursor's position to agree.
Rejected or mismatched caches are rebuilt from Facts; a failed restore leaves
the current cursor intact. The Store's single Session cache slot does not change. The envelope writes metadata and raw payload once without
deep-cloning projected messages. The default provider uses the version-10 fold
encoding below as its opaque payload; other providers own their payload schema.

Exact Fact prefixes with no active model assembler may be encoded as the
version-10 Context checkpoint. Retained nonterminal turns are encoded with their
lifecycle state, so accepted queued turns do not prevent a checkpoint. Context
alone owns and validates that schema, recomputes all message accounting on
restore, and binds the retained projection to the immutable header, exact
retention limits, cursor, and a rolling SHA-256 digest of every folded Fact.
Restore validates semantic metadata bounds and its positions against restored
Turns before accounting or pruning; the provider separately verifies builder identity.
The envelope first measures the borrowed retained projection to admit its exact
encoded allocation, then serializes it into that allocation and prefixes its raw
digest. Checkpoint creation traverses the payload twice without deep-cloning the
retained messages. Fact-prefix hashing streams canonical JSON
directly into SHA-256, and the immutable system message plus its canonical byte
size are cached once per fold.
The Store carries that prefix digest independently so the executor can reject
bytes that no longer describe the canonical prefix. A claim-filtered sequence
hole, active assembler, wrong header, wrong limits, changed payload, or
malformed bytes makes the fold non-checkpointable. The checkpoint is an
integrity-checked cache written by the trusted in-process Context owner, not an
authentication boundary against coordinated replacement of both Store metadata
and cache bytes.

An accepted mailbox turn becomes checkpointable only after its first
model-visible input has entered; Context never writes an empty turn that its
restore boundary would reject.

Provider replay evidence is durable transcript evidence, not a portable prompt
token. Context therefore builds a provider-neutral request without consulting a
generic capability profile: the current AI seam exposes endpoint, configuration
generation, and credential source only after preparation. Until those exact
route facts can be preflighted together, Context never uses replay evidence to
elide canonical history and removes
provider-private reasoning blocks from the next provider request. Visible text,
tool calls, tool results, and the complete retained turn prefix remain. This is
the fail-closed boundary that prevents one deployment's response identity from
crossing into another deployment that accepts the same extension format.

A fork fold owns the complete inherited interval recorded in the child Header.
Seed pages must begin immediately after `resolved_after_seq`, remain contiguous
across page boundaries, and finish exactly at `resolved_terminal_seq` before
child Facts may be projected. The inherited interval must also contain only
balanced completed turns at that boundary. The parent interval does not advance
the child's Fact cursor or Fact-prefix digest.

Checkpoint encoding writes through a private capped writer directly into its
final envelope buffer. The complete envelope counts against the existing byte
bound. The generic builder releases the copied opaque payload before allocating
the final shared envelope. These allocation rules preserve the serialized format,
binding checks and optional-cache failure behavior.

Both fold modes reject orphan Tool results during ingestion; request construction
and compaction also reject misordered results. Legacy non-semantic projection
does not apply builder 2.7.0's semantic pruning. Unit shape discovery performs no JSON byte accounting. Compaction
measures the pruned messages only after shape validation.

Compaction constructs one coordinate remap per affected turn from its ordered,
disjoint selections. Messages, Tool batches and protected coordinates share that
map. Installation checks the current digest, stages replacements, verifies strict
shrink, and completes fallible accounting before mutation. Metadata remaps then
update only affected coordinates in place;
unaffected message vectors, batches and accounting retain their existing ownership.
Replay eligibility and view/source digests remain authoritative, with unchanged
builder and checkpoint encodings.

Program-origin Tool intent/start/result records retain bounded provenance in the
fold and checkpoint but create no provider ToolCall or ToolResult messages. Only
the enclosing model-origin coordinator result enters ordinary Context. Replays
validate exact nesting, unique active ordinals and full Tool identity (owner,
invocation, call and request digest) for both model and Program calls; compaction preserves that
state while its parent call remains retained and drops it with the parent batch.

The service supplies one `ContextBudgetContract`, inherited by every Agent Scope
and retained by overlapping executor generations and checkpoint maintenance.
Its default accounted limit is 512 MiB; pressure fails immediately with
`ContextError::Capacity`. Parking execution releases a lane but keeps its cursor
charged. No cursor eviction or replay policy is implied. The ordinary Host
Profile patch can replace `rsi-context-budget` configuration with a positive
`maximum_bytes`; replacement requires service restart so it cannot create a
second simultaneous pool.

Admission precedes retained-state growth and deep projection copies. Semantic
state, immutable headers, schemas and request options use canonical encoded
weight; assembler content uses an incrementally maintained conservative encoded
weight. Buffer credit follows retained allocation capacity through clone, slice,
prepared-call and Store worker ownership. `ModelContext` exposes its messages by
immutable borrow: projected storage cannot be moved out separately from its
credit. A caller making an explicit deep copy admits that copy independently.
This accounts defined resources rather than RSS or allocator overhead; provider
private media/wire allocations and SQLite pages have their own limits.

The limit accounts retained state and temporary work together. Fact admission
covers the old state, staged replacement and escaping overhead; projections cover
copied messages and normalization, and restore covers encoded input plus decoded
state. Conservative admission policy can refuse work before retained content alone reaches
the configured limit. Each request materialization uses one caller-owned
projection credit. Checkpoint reads reserve only the validated body length at the
Store allocation boundary; queueing and absent checkpoints reserve no bytes.
Restore refusal surfaces as `context.capacity` instead of triggering full-history
replay. Required requests and forced compaction fail on pressure; optional
maintenance and compaction decline with diagnostics. Custom builders receive the
same mandatory budget and keep admission with their allocations.

The Host-selected `maximum_bytes` is any positive host `usize`; no value disables
checked admission. There is no additional fixed ceiling because hosts must size
this shared pool for their chosen concurrency. API and durable-format byte limits
remain independent. A very large configured pool weakens operational protection;
the operator owns that choice, and this accounted-weight policy is not an RSS limit.

The current peak admission formulas use accounted retained fold weight `S`,
encoded Fact weight `F`, options weight `O`, Header weight `H`, and checkpoint
length `C`. Opening a bare fold reserves `2H`; the standard `ModelContextState`
also holds its own Header copy, making the opening peak `3H`. Applying a Fact
temporarily raises the fold's credit to `3S + 8F`; a projection/request adds
`4S + O` while retaining the fold,
so it needs at least `5S + O` in an otherwise empty shared pool. Restore reserves
`3C` for decoding in addition to the still-owned read buffer, existing cursor
and newly opened fold. Replacement retains both wrapper Header copies until
the new cursor is accepted. At the 64 MiB checkpoint bound, the read buffer and
decode workspace alone need 256 MiB; simultaneous old/new cursor and Header
credits reduce concurrency further. Capacity refusal leaves the prior cursor
intact. A planned summary request holds credit for both its request and plan.
Callers must retain the request while holding that plan, until the plan is admitted
as a ModelIntent; moving or copying the plan alone does not carry Context credit.
Other cursors,
requests and diagnostics consume their own simultaneous credit. These are
operation headroom policies, not proven allocator bounds or fixed fractions of usable model context: the
32 MiB materialization limit and configured shared capacity are separate limits.
Idle folds retain `S`, not `3S`; only the current operation raises its own credit.
Pre-admission refusal leaves that fold usable, and release of another retained
owner immediately makes its credit available for retry. Executor claim failure
drops its local cursor rather than holding it indefinitely. The pool has no
automatic eviction, per-Session quota or fairness guarantee. Passing an individual
Fact/checkpoint size validator does not guarantee admission alongside other
owners. Two maximum-sized restores alone can consume the default 512 MiB pool
before Header/cursor credit; one maximum-sized Fact requests 288 MiB plus `3S`.
Deployments needing that concurrency must size the shared pool for simultaneous
operations. A smaller per-Session quota would reject additional legal workloads;
it cannot establish an allocation bound for the current encoded-weight policy.

Accounting reuses the immutable Fact's construction-proven encoded length and
caches immutable Header, assembler bindings and per-Turn metadata.
Restoring a semantic checkpoint validates its builder binding and recomputes all
retained message and Turn accounting; it skips the per-Fact ingestion admissions. A newly enabled semantic state
is accounted before the cursor is returned.
Ordinary ingestion refreshes only the changed Turn. Batch metadata caches each
encoded entry and remeasures only batches mutably accessed since the last charge;
adding a batch does not rescan prior batches. Restore and compaction rebuild that
derived cache from their complete replacement. Streaming deltas reuse unchanged
batch/instruction weights and maximum-width source sequences.
Instruction replacement refreshes only the current Turn and earlier Turns whose
protected instruction coordinates changed; it does not reserialize unrelated
Tool batches. Installed summaries rebuild bounded metadata. Six
encoded Turn identity weights cover owned index keys, while semantic fragments
and batches use measured canonical weights. Accumulated assembler text and
unchanged metadata are never reserialized for each delta.
