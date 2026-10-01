# rsi-ssh-transport

One connection binds an explicit nonzero epoch to an already authorized duplex
byte transport. The client sends requests; the helper replies. Ordinary messages
are at most 2 MiB, with 32 pending calls and separate 8 MiB incoming and outgoing
payload budgets. Fragment assemblers share the incoming budget with delivered
messages until their owners release them. Payload vectors can have at most twice
their charged byte length in allocation capacity. External frame headers are
validated before allocating at most 16 KiB for a body.

Sixteen reserved requests carry only the closed protocol control operations.
Heartbeat and acknowledgement each have their own single slot. An admitted
heartbeat must be acknowledged within 30 seconds even if its waiter is dropped;
expiry retires the connection, without reusing ambiguous heartbeat capacity. The writer checks
heartbeat, controls and pending stream credit before each ordinary/data fragment;
it round-robins ordinary messages and data streams. An in-progress frame write is
never cancelled to interleave bytes. A stopped underlying pipe still requires
connection retirement; priority does not make a blocked byte transport writable.
The helper observes fresh client heartbeat serials; only its lifecycle owner may
use that observation to feed its watchdog.

Every call retains its pending slot after its waiter disappears, until the matching
reply or connection settlement. Retirement reports `OutcomeUnknown` once the
writer has selected the first request fragment for I/O; queued undispatched calls
report `Closed`. Neither case triggers a retry. Replies must match the exact call
class, identity and fragment sequence, and may not precede dispatch of the final
request fragment. Dispatch here means writer selection, not local flush completion:
a correctly correlated peer reply can arrive while flush is still pending. A
reply attests peer receipt; it cannot prove an honest result from a compromised
target. Incoming ordinary and control call starts
use disjoint identity parity and are strictly increasing within their own class.
The helper releases an inbound call slot when its final reply fragment is selected
for writing; the one in-flight frame remains bounded independently. This precedes
local flush completion because the peer can already consume the complete reply
and submit replacement work while flush is pending. Dropping an unanswered incoming
request retires the connection rather than inventing a successful result.

Every admitted request has a 30-second deadline, independent of its waiter and
of heartbeat progress. One connection-owned timer retires the entire epoch if
any request lacks its complete reply at that deadline. Pending calls settle by
their dispatch state; no effect is retried and capacity is not reused on an
ambiguous live connection.

There are 64 stream slots. An identity includes its slot and a strictly increasing
generation; a retired identity cannot be registered again. Only the current
registered generation can publish bytes or credit. Already in-flight Data/Credit
for retired generations are discarded without publishing bytes, returning credit
or touching a reused slot; future unregistered generations retire the connection.
This lets cleanup and slot reuse race safely with previously admitted frames.
Each direction grants four frame credits, including EOF, with exact data and credit sequences. Reading
one frame returns credit; a full or abandoned consumer never blocks the connection
reader or other streams. A credit grant cannot acknowledge unsent data. EOF is an
explicit ordered frame; disconnect is an error, never manufactured EOF. Stream
handles are connection-bound. Dropping a handle does not release its slot or cancel
accepted bytes. The process owner calls `retire_stream` after its cleanup
barrier, or retires the connection. Unstarted plans can abandon unused streams;
started effects require native settlement before abandoning queued output.
Local retirement precedes releasing the peer's reusable slot reservation.
This preserves bounded ownership for unfinished
streams without an unbounded tombstone map.

The byte transport owner must retain and reap its underlying SSH process. The
helper owner validates ordinary request schemas, target handles and authorization,
and bounds handler work independently. This package alone establishes no process,
filesystem, watchdog, hot-launch RPC count or wall-clock latency guarantee.
