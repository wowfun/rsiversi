# rsi-acp

This ordinary connection driver consumes the validated ACP wire contract. Each
peer owns at most 32 pending requests, 8,192 queued incoming messages and independent
8 MiB encoded-payload budgets for each direction (16 MiB combined). Incoming
retention cannot spend the capacity needed for permission replies or cancellation.
Incoming saturation retires the peer; it cannot block response correlation behind
unconsumed notifications. Local frame, byte or write-queue admission failure is
inert before dispatch. Once a request is queued, cancellation means unknown delivery.
Outbound envelopes are assembled from validated in-process values and encoded once
under the frame byte bound; only external bytes use the untrusted-input decoder.

Writes are serialized and acknowledged only after flush. A drain waits for
write-queue capacity, with connection cancellation ending that wait. A drain barrier passes
all earlier writes and is required before a load response. Request cancellation
or response timeout retires the connection: delivery is then unknown and never
automatically retried. Unknown response IDs and malformed frames also retire it.
An already flushed permission request is the exception: abandoning its local
waiter retains its exact correlation slot until the peer replies or disconnects.
The existing 32-request limit includes these slots. Late permission replies are
discarded and cannot authorize a cancelled prompt; no request is resent.
Prompt deadlines belong to the application; initialize uses ten seconds and
control calls thirty seconds. Cancellation settlement/drain uses thirty seconds.
Incoming messages have monotonic connection-local ordinals. Each correlated
response records the preceding incoming-message horizon; consumers must process
through that horizon before publishing replay completion or a prompt's final
projection. Correlation alone does not mean a consumer has drained its queue.

`Peer` owns the reader and writer tasks. Its explicit close cancels, joins and
closes transport; dropping the owner cancels transport work. Detached product UI
controllers retain the Host-owned Peer rather than owning its lifecycle.
The Process adapter retains `ManagedDuplexProcess`; Process performs termination,
pipe shutdown and reaping. The driver neither starts ambient commands nor owns
an alternate subprocess mechanism.

`server::run` dispatches stable agent methods to an application-owned backend.
It requires initialization, rejects unsupported methods and validates raw setup,
prompt and cancel parameters before DTO conversion. It admits at most 32 handler
tasks, bounds initialize/control operations to ten/thirty seconds, and leaves
prompt duration to native execution policy. Load backends must finish complete
frozen-horizon replay before returning; the router then crosses the writer drain
before sending the response. Backend shutdown owns cancellation and settlement
of admitted native work, even when a caller or handler future disappears.

Transport close reports settlement failure. In particular, a Process wait error
cannot become a successful peer close or a Closed external conversation. Retiring
a connection still joins both driver tasks and attempts the transport cleanup.

Handler first polls follow incoming wire order before concurrent continuation.
A prompt backend must synchronously admit or reject its operation before its first
await. Consequently a following cancel observes that admission even on a
multithreaded executor; task scheduling cannot invert these protocol effects.
