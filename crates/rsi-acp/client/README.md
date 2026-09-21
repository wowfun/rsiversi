# rsi-acp-client

This library owns one external ACP Session over an explicitly supplied `Peer`.
The Host keeps its `Client` owner across UI detach; detachable handles do not own
process lifetime. Endpoint configuration, credentials and Process/Sandbox launch
authority belong to the Host integration. One peer serves one external Session.

The caller reserves a journal identity and connection generation before attaching
the peer. Initialization admits only stable version one and explicit advertised
capabilities. Reconnection is an explicit resume/load decision, never an automatic
prompt resend. Unknown send, peer loss or uncertain cancellation remains Unknown.

The incoming route exists before any new/load/resume request. Every accepted
update is journaled under its local connection generation and replay epoch.
Response completion waits until the consumer has processed its exact preceding
incoming-message horizon. Full load publishes a replacement epoch only after
that barrier; resume does not fabricate or replay missing history.

One prompt is admitted per Session. The owner retains its task if a caller drops
the submission waiter. The prompt owner orders cancellation after the admitted prompt
write and waits at most thirty seconds for the actual response. Cancellation before
wire admission records Discarded without sending either message. Completion retains
the exact stable stop reason; token exhaustion and refusal are not objective success.
Close joins prompt and reader cleanup and closes the supplied transport. Process
reaping is delegated to `ProcessTransport`, never reimplemented here.

Permissions preserve all four standard option kinds and exact option IDs. An
answer is bound to the local conversation, connection generation and retained
peer request. Allow-always creates no local grant. Missing, stale or invented
choices are rejected. Unsupported filesystem, terminal and elicitation requests
receive method-not-supported; no ambient authority is acquired by the client.

Setup rechecks connection admission after the durable bind, under the observation
publication lock. A setup response followed by EOF cannot replace the reader's
Unknown settlement with Ready. Confirmed prompt completion remains distinct from
connection availability.

Initialization accepts up to eight ordered, distinct select-option assignments.
IDs and values are nonempty, NUL-free and at most 256 bytes. Each choice must be
advertised in the latest setup/configuration response. Both flat and grouped ACP
select choices are supported; boolean and unknown kinds are not coerced to strings.
The complete sequence has one thirty-second deadline and each reply must confirm
all assignments applied so far. At most 64 options and 4096 choices are admitted
per response, in addition to the wire frame bound. Setup failure does not admit a
prompt; failed load configuration preserves the previously visible replay epoch.

The incoming consumer batches already queued adjacent updates (64 records / 1 MiB),
retaining their byte leases until commit. It advances the processed horizon only
after the entire batch commits and never batches across a permission request.
