# rsi-api-browser-probe

This standalone fixture exercises public API registry and Meta plugin ownership
inside real Dedicated Workers. It checks exact JSON and last-clone byte admission,
mutation waiter drop, device quotas, delivery leases, finite/SSE decoding, read
retirement, plugin withdrawal, and the shared negotiated client owner over a
scripted transport using
the browser Execution backend. Two Workers execute independently in Chromium and
Firefox. This is API foundation evidence; it does not claim an HTTP connection,
application rendering, or live provider behavior.

Run `npm ci`, `npx playwright install chromium firefox`, then `npm test` here.
The toolchain requires `wasm32-unknown-unknown` and wasm-bindgen CLI 0.2.127;
`RSI_WASM_BINDGEN` can select an explicit executable. The shared
[runner](../../tools/README.md) serves a fixed asset allowlist without credentials
or access to user state. The fixture retains independent Cargo and npm lockfiles.
