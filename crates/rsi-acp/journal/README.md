# rsi-acp-journal

`position` reads one exact visible epoch's highest observation sequence without
materializing payloads. Replay publication fences the epoch; an obsolete epoch
returns `Stale` and cannot be mistaken for a reading position in the new history.

This store owns locally observed external ACP conversation history. It never
opens the Agent Store or writes native Agent Facts. The product supplies its
dedicated `state/acp` directory. Schema 1 is exact: unknown versions and unrelated
databases fail without migration, reset or replacement. One exclusive file lease
covers all admitted operations, including work whose caller has disappeared.

The owner admits two blocking jobs. Each append batch retains at most 64 records / 1 MiB total;
history pages contain at most 64 records and 256 KiB. A record too large for the
remaining page is returned through explicit 64 KiB byte windows. Records retain
exact local sequence and replay epoch; a replay is staged in a new epoch and
becomes the visible projection only after the remote load response succeeds.
Failed replay never masquerades as a complete replacement.

The journal limits observed data to 64 MiB per conversation and 1 GiB per owner,
including metadata reservations for at most 4,096 saved conversations. It reserves
16 KiB per conversation for settlement metadata before admitting observations.
SQLite also has a page-count limit; quota exhaustion is explicit. Terminal status
updates do not consume the observation quota. A crashed Running/Starting/Loading
conversation reopens as Unknown, retaining its last confirmed remote identity.
Opening validates the stored accounting against record lengths before accepting
new writes. This scans the bounded journal metadata; it is not constant-time
startup. The database page ceiling reserves room for the rollback image and its
overhead, so physical storage can exhaust before the logical payload limit.

The journal stores endpoint IDs, workspace directories, negotiated capabilities,
user input, received updates and categorical settlement with the exact stable
prompt stop reason. Discarded means local cancellation before wire admission;
Cancelled requires an actual peer prompt response. It never stores endpoint
commands, environment credentials or an assertion of remote durable acceptance.
Successful local writes prove only that these observations were stored locally.

Explicit retirement closes admission and retains an owned cleanup task until all
admitted blocking work returns. It then closes SQLite and releases the lease even
if an obsolete service handle remains retained. Dropping the retirement waiter
does not cancel that cleanup. Old handles cannot read or write after retirement.

Reads use immediate admission to the two blocking workers. Durable mutations
reserve one of 32 bounded waiter slots before awaiting a worker, so eight resident
peers can settle concurrently without losing their final observation to ordinary
worker contention. Excess mutations return Busy. Retirement rejects queued work
that has not acquired a worker; admitted blocking work retains its worker and lease.

Failed or interrupted replay removes its unpublished epoch and refunds its quota
atomically; sequence numbers and epoch counters are never reused. Startup accepts
sequence gaps left by discarded replay. Adjacent incoming updates may commit in
batches of at most 64 records and 1 MiB encoded payload, without crossing permission
requests or completion horizons. Owner accounting is initialized once from validated
rows and maintained transactionally for the lifetime of the connection.
Append checks the generation alongside sequence, epoch and accounting columns;
it does not decode the unchanged settlement metadata for each streamed record.
