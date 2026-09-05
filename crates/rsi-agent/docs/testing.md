# rsi-agent testing

Protocol suites exercise constructor and deserialization rejection, exact
round trips, Agent preset identity grammar and required Header membership,
sequence invariants, byte limits, and dependency direction. The
Memory testkit proves append/read behavior, Store-level turn-lifecycle
admission, open-session presence and closure, checkpoint replacement,
aggregate-byte pagination, and pre-commit failure injection.
SQLite integration tests separately cover session and per-turn pagination,
open-turn indexing and cursor pagination, optimistic conflicts, rejection of
representative old schemas, index integrity,
CAS integrity, fast reopen with dormant corruption, first-access rejection,
explicit full verification, reader/writer WAL snapshots, exclusive writer leases, and direct database
tampering with header or Fact rows above their framing bounds. Checkpoint-row
tampering also proves reads reject a mismatched immutable-header fingerprint
or a cursor beyond the durable session tail before returning opaque bytes.

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
Draft and composition tests cover default and explicit selection, failed
replacement preserving the prior pin, drop without Store state, single-flight
generation construction, source-digest replacement, resident old-generation
stability, cold-resume rebinding, broken-source failure before admission, and
last-pin reclamation. Kernel tests prove the fresh pin moves exactly once into
resident state, resume tokens preserve resident pins, token failure/drop
releases cold pins, and every claim returns the exact admitted pin. Standard
application tests also prove generation preparation precedes durable Workspace
registration for both fresh and resumed sessions.

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
