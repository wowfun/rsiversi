# rsi-agent testing

Protocol suites exercise constructor and deserialization rejection, exact
round trips, sequence invariants, byte limits, and dependency direction. The
Memory testkit proves append/read behavior, Store-level turn-lifecycle
admission, aggregate-byte pagination, and pre-commit failure injection.
SQLite integration tests separately cover session and per-turn pagination,
open-turn indexing, optimistic conflicts, schema rejection, index integrity,
CAS integrity, reopen recovery, exclusive writer leases, and direct database
tampering with header or Fact rows above their framing bounds.

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
Race tests also prove a claim read cannot skip a prefix committed while Store
I/O is in flight, and repeated invalid resumes of historical idle sessions do
not consume the resident-session bound. Cold outcome lookup and recovery prove
they do not read unrelated session Fact pages, while concurrent resumes prove
that one session has at most one in-flight control-state load.

Context tests fold real Facts and prove deterministic compaction, complete-turn
removal, tool call/result adjacency, Media references, and hard byte/message
bounds. Executor tests cross actual Local contracts and inject provider/Tool
implementations to prove intent/start durability before I/O, successful and
failed Tool-result retirement, interleaved same-session submission, retry
admission, Approval plus Sandbox policy propagation, retained Tool settlement
without a shorter recovery timeout, and shutdown release only after an aborted
worker has joined. Direct Image
tests commit multiple Media refs one at a time and force a tail failure to prove
`partial_failed` preserves the durable prefix without media bytes. Kernel tests,
not the executor suite, own durable interruption repair and cancellation races.

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
