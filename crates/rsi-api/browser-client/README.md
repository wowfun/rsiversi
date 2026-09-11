# rsi-api-browser-client

This connection plugin runs in a Dedicated Worker without WASM atomics. It
publishes the shared Rust ApiClient using same-origin Fetch and the platform's
HTTPS trust. Configuration contains an expected EndpointId and an explicit
loopback HTTP development opt-in. The origin comes from the Worker; no remote URL
or credential is serialized into the Profile. Existing HttpOnly cookies authorize
calls, with exact Origin and CSRF enforcement by the HTTP endpoint. Cookie login
and logout are explicit operations. RSI issues each Fetch once and does not
automatically replay mutations or reauthenticate.

Before publication the client reads `connection.caller` and requires a Device
identity. Subsequent requests pin that authenticated device using the HTTP
expected-device fence. A different tab changing the shared cookie cannot make
an old connection execute as the replacement device. Explicit logout retains
the same expected device even after logical connection cleanup.

HTTPS browser connections require the endpoint to report actual HTTP/2 negotiation.
HTTP/1 browser connection pools cannot preserve control progress once idle
subscriptions occupy every socket. Explicit loopback HTTP remains a transport
debugging mode with that limitation; multi-surface product delivery requires TLS
and HTTP/2. Native HTTP clients retain their independent transport policy.

The shared connection owns negotiation, operation admission, immutable response
retention and retirement. Worker-local tasks own Fetch, AbortController and BYOB
readers. Safe Rust channels transfer bounded metadata and bytes; no JavaScript
handle crosses a Send/Sync interface. At most 64 local tasks exist per transport.
Each task retains its slot until pending Fetch/read/cancel promises settle.
Dropping a response aborts that local request. Closing the plugin first drains
logical connection work, then waits up to five seconds for platform tasks; an
unsettled task reports cleanup failure rather than successful withdrawal.
The factory installs that owner before negotiation starts and tracks the
negotiation task. Cancelling activation aborts its transport, joins that task
and waits for platform settlement before reporting clean cleanup.
`BrowserClientFactory::connect_in` gives composing plugins the same unpublished
connection owner without duplicating the lifecycle protocol.

Local Fetch cancellation is not an application-level remote acknowledgement.
Transport tests independently observe remote read/stream release; they must not
enable browser request interception, which can change cancellation behavior even
for requests outside the intercepted route.

Bridge source copies, JavaScript receive windows and Rust chunks each have
independent control/data pools of 2 MiB/64 MiB. Admission precedes their allocation.
BYOB reads supply at most 64 KiB and validate the returned view before copying.
There is one requested chunk at a time, with no unsolicited body queue. Retained
payload decoding uses the same finite/SSE drivers as the native client. These
bounds cover explicitly owned bridge allocations, not browser internal HTTP/TLS
buffers, garbage collection or process RSS.

Requests reject redirects and cross-origin access. Response identity, content
metadata, status and frames use the shared Rust validators. Response URL and
bounded header strings are checked before exposure. A possibly dispatched
mutation with a lost or malformed response reports OutcomeUnknown, without an RSI
retry. A single Fetch may send multiple HTTP requests through the browser's
internal connection recovery, as the header-loss fixture demonstrates. Fetch
does not establish at-most-once server delivery. Domain identities and revision
checks remain authoritative for duplicate handling; cookie exchange repeats only
the same cookie assignment or removal.
Negotiation has a 15-second deadline, finite exchanges one minute, and explicit
stream end requires EOF within five seconds. Idle subscriptions have no timer.

Browser verification must execute the real Fetch adapter against isolated HTTP
listeners in Chromium and Firefox, including cookie authority, raw binary data,
stream cancellation and platform cleanup. Compilation and scripted body sources
alone do not establish those properties. Worker traps cannot acknowledge cleanup.
