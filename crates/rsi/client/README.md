# rsi-client

Shared Rust application control logic consumes domain contracts and an explicit
Execution. It does not own a Runtime, network transport, terminal, DOM or backend.
Application/controller plugins own the futures they drive and their cancellation.

SessionControllerFactory is an ordinary plugin requiring the Session domain and
an ObservationSink capability. Its bounded configuration names one SessionId and
an optional initial cursor. An attached surface supplies its inspection cursor;
a fresh draft omits it and starts observation from origin after the first accepted
message. The factory attaches only within its own Context's Session capability.
Each surface isolates its controller and sink contracts while inheriting its
connection/domain capabilities. Session-free application compositions omit these
plugins entirely.

The controller owns at most four non-queued message submissions and one pair of
observation tasks. Calling submit admits and schedules reconciliation before
returning a waiter. Dropping the waiter leaves that work owned; controller
retirement fences submissions, cancels both observations and drains submitted work.
An application may explicitly supply a reconciliation-stop token with
`submit_cancellable`. Cancelling it ends client reconciliation with the original
Session/Message unknown-outcome identity; it cannot cancel an admitted domain
mutation. Dropping its waiter alone still leaves reconciliation owned. This lets
an interrupted application drain its controller without waiting on an unavailable
service or starting a replacement submission.
An escaped controller rejects new work after withdrawal. It never cancels a
remote Session or stops the service. Observation failures reach the sink's stopped
callback with the failed stream kind; the renderer owns the recovery presentation.
Controller attachment uses the bounded read-capacity policy below while its
enclosing surface holds an admission slot.

Message submission retains the caller's complete input and MessageId. An unknown
outcome first queries that identity. Only an authoritative NotFound permits one
resubmission of the identical input; a second unknown outcome permits one final
query. Other query failures preserve the unknown outcome. This policy uses Session
durable identity, not a transport replay or a generic outcome ledger. Explicit
controller retry of a retained unresolved request begins with a status query;
query failure sends nothing, NotFound permits the same reconciliation sequence,
and an accepted receipt starts observation even when no replay was necessary.

Session extension commands use the same controller's four non-queued work slots.
Discovery returns the pinned catalog and exact predecessor; preparing an invocation
selects a descriptor identity and freezes bounded JSON arguments, the caller's
request ID and that predecessor. Interactive slash syntax uses a JSON string for
the text following the name. Only registered names intercept message submission;
unknown slash names remain Human text for the existing workspace-skill resolver.
`//` remains ordinary message text. Structured callers may
provide any validated JSON arguments.

An execution sends the frozen invocation once. An unknown outcome queries that
same request ID once and validates the receipt's complete invocation digest.
An absent receipt can mean an admitted callback is still running, so neither
automatic reconciliation nor explicit result refresh sends another mutation.
Applications retain unresolved invocations across pane changes; they display
the original identity and keep result refresh available. A revision conflict is
a completed rejection, and a new explicit action captures a new predecessor.
Controller retirement cancels local command waits with their original unknown
identity; admitted server work retains its own owner.

`CommandSubmission` stores one unresolved bounded invocation and the last compact
receipt per saved Session. A dropped application waiter preserves that invocation.
Its separate single-work admission prevents a new command from replacing unknown
input; result refresh preserves it even if the current controller is unavailable.

`drive_message` follows one submitted message through its durable claim and the
claimed Turn's terminal Fact. It emits typed receipt, claim, Fact and outcome
events to an explicit sink; the caller owns the future and presentation lifetime.
An interrupt first attempts to cancel an unclaimed message. If that loses the
claim race, it still cancels the exact claimed Turn. A discarded message reports
its reason unless this caller requested cancellation. Terminal Fact and outcome
delivery remain part of completion even after an interrupt. Renderers own output
formatting and may stop presentation independently; the driver owns no terminal,
DOM, task scheduler or interaction subscription.

Interactive reads may overlap other finite reads whose maximum reply reserves
the same bounded API lane. `read_with_capacity_retry` retries only typed Session
or API Capacity errors, up to five attempts with 50, 100, 200 and 400 ms delays.
The caller must supply a read operation and own its bounded request slot and
cancellation; the helper creates no task or queue. Each underlying operation
retains its own I/O deadline. Other errors, including unknown outcomes, return
immediately. Mutations use their domain-specific reconciliation policy.

Fact/control and interaction observation share one retry policy: 250 ms initial
delay, doubling to 2 seconds; successful delivery resets both delay and failure
count. Capacity failures, including API Capacity, do not consume the five-failure
cutoff. Other consecutive failures terminate on the fifth. Reconnection notices
are typed values delivered through the same explicit sink; presentation belongs
to the renderer. A stopped sink ends observation without another reconnect.

The Fact/control cursor advances only after the sink confirms delivery of that
record. A durable watermark can exceed the delivered sequence and never advances
this cursor. Applications supply an origin cursor for fresh drafts or the exact
inspection cursor used to render an attached Session's snapshot. Interaction
streams deliver replacement snapshots rather than polling individual questions.
Dropping the observation future releases its stream and timer; it does not cancel
an admitted server mutation or terminate the connected service.

The shared production closure builds for native and wasm32 without terminal,
native backend or UDS dependencies. Native application regressions exercise the
controller through ordinary Shell-owned terminal surfaces. The [Worker probe](../../../fixtures/rsi/client-probe/README.md)
runs the same controller scope, submission-drain and acknowledged-cursor scenarios
in Chromium and Firefox. Product Web rendering and real domain transport integration
remain separate validation surfaces.
