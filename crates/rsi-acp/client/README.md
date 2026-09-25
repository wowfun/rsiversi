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
that barrier and in the same journal transaction as the confirmed binding;
resume does not fabricate or replay missing history.

One prompt is admitted per Session. The owner retains its task if a caller drops
the submission waiter. The prompt owner orders cancellation after the admitted prompt
write and waits at most thirty seconds for the actual response. Cancellation before
wire admission records Discarded without sending either message. Completion retains
the exact stable stop reason; token exhaustion and refusal are not objective success.
Close joins prompt and reader cleanup and closes the supplied transport. Process
reaping is delegated to `ProcessTransport`, never reimplemented here.

Permissions preserve all four standard option kinds and exact option IDs. An
answer is bound to the local conversation, connection generation, active prompt
and retained peer request. Completion closes permission admission under the same
lock as answer admission before publishing its terminal snapshot. Late requests
receive cancellation. Allow-always creates no local grant. Missing, stale or invented
choices are rejected. Unsupported filesystem, terminal and elicitation requests
receive method-not-supported; no ambient authority is acquired by the client.
These automatic replies retain the unadmitted request across transient output
capacity, waiting for writer drain within one thirty-second deadline. An admitted
reply is never replayed; timeout or uncertain transport failure retires the peer.

All snapshot transitions serialize eligibility, durable mutation and publication
in a retained owner task, including setup and replay. Dropping a waiter cannot
release this ordering while a durable worker still runs. Setup rechecks admission
before and after binding. EOF settles unfinished work as Unknown; a validated
prompt response with its processed incoming horizon may still confirm completion.
Permission cleanup and transport close failures cannot demote that completion;
they still retire the connection and report cleanup failure. Confirmed completion
remains distinct from connection availability. Close joins
operations and the reader without holding the transition gate. Dropping the close waiter retains the close owner. Resource joins do
not abandon the reader at the control-message deadline. Disconnect during Loading
atomically discards the unpublished replay and refunds its quota.

Permission answers consume a pending choice only after synchronous wire admission.
Capacity returns Busy with the exact choice still answerable. Admitted responses
are never restored or resent; their retained flush task retires the connection on
failure or a thirty-second deadline, even if the caller drops its waiter. Batch
cancellation has one thirty-second flush budget. Partial admission failure retires
the core peer synchronously, including its queued writes, before removing remaining
choices. Flush timeout also retires the core peer even if the incoming consumer is
blocked. This records a categorical peer failure without demoting confirmed completion.

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

Prompt completion closes permission admission and settles pending peer requests
even if durable completion journaling fails. The journal error remains the primary
operation failure.
