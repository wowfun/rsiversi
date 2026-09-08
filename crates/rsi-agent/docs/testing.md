# rsi-agent testing

Protocol suites exercise constructor and deserialization rejection, exact
round trips, Agent preset identity grammar and required Header membership,
sequence invariants, byte limits, and dependency direction. The
Memory testkit proves append/read behavior, Store-level turn-lifecycle
admission, open-session presence and closure, checkpoint replacement,
aggregate-byte pagination, and pre-commit failure injection.

Human steering scenarios run through the Kernel against both Memory and SQLite:
idle routing, active Step consumption, terminal/recovery promotion, immutable
same-ID retries, acceptance FIFO around fixed follow-ups, and explicit message
cancellation. SQLite scenarios also run its offline canonical/index verifier.
SQLite integration tests separately cover session and per-turn pagination,
open-turn indexing and cursor pagination, optimistic conflicts, rejection of
representative old schemas, index integrity,
CAS integrity, fast reopen with dormant corruption, first-access rejection,
explicit full verification, reader/writer WAL snapshots, exclusive writer leases, and direct database
tampering with header or Fact rows above their framing bounds. Checkpoint-row
tampering also proves reads reject a mismatched immutable-header fingerprint
or a cursor beyond the durable session tail before returning opaque bytes.
Cancelled blocking waiters retain the writer lease through actual reader,
writer and CAS completion. Metadata scans above the validation-cache capacity
must not validate history. Large Fact page boundaries and count lookahead
assert that the next JSON body is not materialized. Subtree snapshots reject
cycles and oversized lineage; transaction guards detect children inserted after
a caller's snapshot, including children created by that same commit. Cold subtree
reads and quiescence guards reject a descendant with a missing turn index; warm
reads reuse the established proof. Stored-length admission also bounds control
pages before their next JSON body is materialized.
The report-only Store benchmark measures metadata and first validation before
any mixed writes. Each mixed sample appends to a different session at the same
initial Fact count, so a growing target history does not confound that phase.
Separate operational matrices traverse every member of 257- and 512-session
working sets over repeated cycles and read a fixed one-Fact page against long
control histories. Report latency distributions and actual API call counts;
unit tests own exact decode counts and deterministic lane-blocking assertions.
Concurrent warm reads are sampled during cold validation without a pass/fail
timing threshold or a claim that process memory is OS-cold.

Headless child-process tests drain bounded stdout and stderr from spawn, record
durable acceptance, provider entry, and signal stages, and race stage waits
against child exit and the existing deadline. A timeout captures diagnostics
before killing and reaping the child; partial output is never lost by dropping
an `output()` future. Captured-output overflow fails the test explicitly.

Kernel tests use deterministic clocks and a controllable Store. They cover lazy
empty sessions, live-before-durable observation, the 200 ms batching boundary,
ordered retry after flush failure, cancellation races, final flush, startup
interruption repair, terminal visibility only after its prefix is durable, and
the prohibition on replaying uncertain effects. Persistent flush failure must
also enter an already-attached observation, and shutdown snapshots its flush
waiters so concurrent terminal-session eviction cannot fabricate a missing
session. The same latch rejects later submissions to the failed session.
Recovery preserves durable cancellation classification, and direct execution
tests prove a start cannot share its undurable intent publication. Shutdown
waits on every captured session even when one fails, and a
claim-horizon test submits a later private prompt before the first claim and
proves that prompt is absent from the earlier model request.
An executor integration test installs an unfiltered checkpoint covering two
queued acceptances before the first queued turn is claimed, then proves that
the claim replays its own acceptance and excludes the later prompt.
Race tests also prove a claim read cannot skip a prefix committed while Store
I/O is in flight, and that a later turn accepted during that I/O cannot cross
the claim's already captured live horizon. Repeated invalid resumes of
historical idle sessions do not consume the resident-session bound. Capacity rejection leaves turn control
unchanged and succeeds after the admitted prefix is flushed; checkpoint Store
I/O failures remain typed at the executor-facing seam, and mutated terminal
claims cannot invoke checkpoint maintenance. A tightened Store-read
budget proves checkpoint maintenance declines both unfiltered rebuild reads and
writes instead of producing an unreadable cache. Cold outcome lookup and
recovery prove they do not read unrelated session Fact pages, while concurrent
resumes prove that one session has at most one in-flight control-state load.
Agent-control regressions exercise the complete running/parked/resumed/waiting
Store vocabulary, recovery from shutdown during a durable park, Fresh mailbox
admission racing a write-behind Header, exact idempotent claim receipts with
workspace background Facts, serialized cancellation against direct commits,
post-activation claim handoff, and idle-session capacity reclamation.
The shared Store contract also proves typed activation/quiescence guard failures
and backend-equivalent rejection of duplicate task, message, and activation
identities. SQLite verification separately rejects a fabricated claimed state
for a message whose canonical control stream never claimed it.
Controlled barriers cover source-claim retirement, executor replacement,
cancelled commit waiters, terminal drain, and shutdown timeout while admitted
mutations remain in flight. Exact spawn retries wait for the original admitted
creation and recover its receipt; mismatched messages or lineage are rejected.
Activation terminal tests reject publication that bypasses atomic settlement.
Staging a large publication releases global state while retaining the same
session's admission. Control-tree selection rejects failed history validation
even when metadata remains readable. Independent ready roots progress while one root's
preparation is blocked, with at most four preparations. A permanently failing
first waiting root cannot starve later pages or ordinary flushing; failed
enumeration backs off and health clears only after a complete healthy scan.
Workspace tests retain four blocked jobs across cancellation and generation
withdrawal, and distinguish aggregate byte rejection from complete small
invocation results.
Draft and composition tests cover default and explicit selection, failed
replacement preserving the prior pin, drop without Store state, single-flight
generation construction, source-digest replacement, resident old-generation
stability, cold-resume rebinding, broken-source failure before admission, and
last-pin reclamation. Kernel tests prove the fresh pin moves exactly once into
resident state, resume tokens preserve resident pins, token failure/drop
releases cold pins, and every claim returns the exact admitted pin. Standard
application tests also prove generation preparation precedes durable Workspace
registration for both fresh and resumed sessions.
Composition tests also run beneath explicit service Local isolation. Generation
contributions must inherit that mapping while their pins remain independent of
composition-provider retirement; acquiring a fresh Runtime root would violate it.

Context tests fold real Facts and prove deterministic compaction, complete-turn
removal, tool call/result adjacency, Media references, and hard byte/message
bounds. Fork tests reject child Facts until the complete balanced seed interval
has arrived, and checkpoint tests reject an accepted mailbox turn before its
first model-visible message has entered. Executor tests cross actual Local contracts and inject provider/Tool
implementations to prove intent/start durability before I/O, successful and
failed Tool-result retirement, publication of successful parallel siblings
before propagating a failed sibling, interleaved same-session submission, retry
admission, Approval plus Sandbox policy propagation, retained Tool settlement
without a shorter recovery timeout, and shutdown release only after an aborted
worker has joined. Direct Image
tests commit multiple Media refs one at a time and force a tail failure to prove
`partial_failed` preserves the durable prefix without media bytes. Kernel tests,
not the executor suite, own durable interruption repair and cancellation races.
Executor Tool tests use two different immutable catalogs and prove schema
projection, prepare, retained query/wait/commit, delayed retirement, and
elapsed-budget cleanup never cross their claim generation.
The deadline selector has a deterministic simultaneous-readiness regression:
an already-terminal drive result wins over elapsed cancellation in the same
scheduler poll.
Pool tests use controllable provider gates to prove different Sessions progress
concurrently, one Session remains ordered, the configured peak is respected,
and a lane failure cannot run shared cleanup while another lane is settling.
Checkpoint scheduler tests hold one request in flight and prove per-Session
latest-value coalescing, cross-Session FIFO, capacity behavior, non-starvation,
and bounded shutdown. Closing admission rejects later requests but drains every
request accepted before closure, including an in-flight Session's coalesced
successor, without treating the optional cache as durable truth.

Plugin lifecycle tests use `rsi-meta` Contexts. They verify every ordinary
factory's exact hard dependencies, publication and withdrawal behavior, and
generation replacement. The standard `rsi` package's Headless end-to-end tests
own product composition, SQLite, output, signals, exit codes, Jobs shutdown,
Profile selection, and workspace identity. Jobs tests prove cooperative
cancel-all, bounded timeout, closed admission during finalization, exact scope
isolation, and that the Headless finalizer changes the sole terminal outcome
before its Fact is published.
Executor evidence includes a non-returning third-party finalizer and proves the
executor deadline converts it into the sole durable failure. Reclaiming a turn
whose completed Model event could not be followed by a terminal Fact fails it
as interrupted rather than dispatching another model effect.

Default tests are isolated from credentials, real user state, and live network
services. Native Windows and macOS behavior is reported only by their native
runners; Linux validation does not imply that coverage.
Fixtures pass canonical temporary workspace authorities to production code;
platform temporary-directory aliases must not accidentally become the symlink
under test. Explicit malicious-link cases retain their original test paths.

Human-interaction tests park a real Kernel activation, advance its injected clock
by a day, and block executor admission during resume. They fill the released
tree lane with children and cancel a blocked resume, proving that cleanup releases
its mutation lease. The real executor also runs the human-wait seam with a single
lane while another Session completes. Built-product tests exercise exact prepared
approval metadata, identical answer retries, denial without Tool start, root-only
questions, same-Host reconnect, answer-draft interruption, restart repair, and
continuous CLI observation across two Turns. A one-permit Store-read fixture
prevents regression of deadlock between suspended observation and interaction
refresh. Completed-output tests carry a nonzero Bash exit and truncated text
through context, then request raw UTF-8 boundary pages through `output_read`.
