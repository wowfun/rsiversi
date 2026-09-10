# rsi-api-browser-fetch

This isolated fixture executes the production Rust browser connection against a
real native API listener in Dedicated Workers on Chromium and Firefox. The page
navigates directly to the listener's JSON error document to establish its origin;
local WASM/glue becomes a Blob Worker through browser evaluation. No network route
is intercepted. API routes use actual Fetch, Origin checks and HttpOnly cookies. No user token,
keyring, persistent state or live provider is used.

The shared fixture operations expose raw binary echo, exact integer JSON,
explicit-end events, idle observations, gated mutations and observable counters.
These public seams verify cookie negotiation, mutation waiter independence,
no replay, class admission and browser promise cleanup. Separate browser contexts
own their cookies, and shutdown drains the native server before process exit.
This establishes transport behavior, not product Web rendering or visual quality.

The pre-header read case requires local promise settlement, zero bridge resources
and independently observed remote handler release. Repeated idle subscriptions
must also release their remote admission. Playwright route interception changes
Firefox cancellation even for unmatched API routes; it is prohibited here.
The isolated reproduction and correction are retained in local implementation
notes. Cookie flags and missing/foreign Origin or CSRF rejection
are checked separately through the browser context and actual HTTP listener.

Run `npm ci` and `npm test` here with the matching WASM binding CLI available as
`RSI_WASM_BINDGEN`. The locked fixture owns its Rust and browser dependencies.
The default run covers explicit development HTTP and TLS/HTTP2, including eight
idle subscriptions with a responsive control operation and subsequent remote slot
release. Only this fixture bypasses browser certificate trust for its test PEM;
the native TLS tests still validate explicit certificate trust independently.
Two simultaneous Workers also hold four subscriptions each and meet at a server
barrier while all eight remain admitted. Their controls and cleanup run through
the same TLS listener. A separate bounded fault listener tests 22 malformed finite
responses, five invalid SSE bodies and a lost login result; it obtains its exact
operation catalog from the Rust fixture. A transparent counter around the native
Worker Fetch verifies exactly one application dispatch per operation. The server
counts actual HTTP deliveries separately: a completely missing response head can
trigger browser-internal retransmission even for POST. Other malformed responses
must still arrive once. The fixture reports those retransmission counts and
requires OutcomeUnknown for possibly dispatched mutations and cookie exchange.
