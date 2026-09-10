# rsi-ui-api

Authenticated UI operations bind a semantic export scope through an explicitly
injected `UiTargetBinder`. Only trusted `ApiContext.origin` enters that binder;
request JSON cannot supply a Context, Local contract key or caller authority.
Each binding owns a real Meta target and its domain dependencies. Its owner
retires synchronously and joins admitted actions before closing those dependencies.
The generic adapter imports no Session or application implementation.

Catalog binding is temporary and returns logical bundle/surface names in pages
of at most 64 entries. Continuation uses logical names and grants no authority. Observe
binds fresh targets and multiplexes up to 16 presentations in one subscription
per authenticated origin and application nonce. There are at most 16 such
applications per adapter, also subject to registry and UI admission. A nonce
is a lifetime/replay fence, not an authentication credential. Disconnect,
revocation and registration retirement close presentations, drain admitted UI
actions, then release target owners. Failed initial binding releases all earlier
bindings. Observation holds no domain read lease between snapshots.

Each item carries a full validated model and one optional current input ticket.
Tickets use the existing native/Worker 128-bit entropy source, so receiving an
earlier item cannot predict an input ticket for a model not yet delivered.
The ticket is bound to trusted origin, application, presentation epoch and displayed
revision. The server atomically consumes it before UI action admission, including
capacity failure. Only the current ticket and busy bit are retained; duplicate or
stale tickets return unknown outcome without executing again. No mutation is
automatically replayed. API dispatch admission and authentication precede body
reception; failures there cannot consume a ticket which has not been received.
After settlement, a new item may issue a fresh ticket for the current model.
Diagnostic-only status changes preserve an unconsumed ticket and do not resend
an identical model/ticket pair. Revision changes and consumed-ticket settlement
still publish the current input state, including the absence of a ticket while busy.

One stream item occupies at most 132 KiB including its envelope; snapshot byte
and count leases remain attached to transmitted JSON until its last reader drops.
The stream reserves a bounded handoff slot and output bytes before materializing
an item. Source reads use a separate binary window of at most 64 KiB, checked
against the exact presentation/revision and exposed source name. Bindings and
application slots remain owned through asynchronous retirement, even if the
caller drops the response waiter.

Run `cargo test -p rsi-ui-api` for public dispatch, replay, retirement and retention
checks. Product target bindings, native module and browser tests provide separate
integration evidence.

`UiClient` requires all exact operation descriptors from its negotiated API client.
It validates received model identities, monotonic presentation revisions and source
windows. It sends each action once and preserves transport unknown-outcome errors;
automatic reconnect or mutation replay is never hidden in the client adapter.
