# rsi-client-browser-probe

This standalone fixture runs the same public controller-plugin scenarios as
`rsi-client` native tests inside two independent Dedicated Workers in Chromium
and Firefox. It observes real Meta child scopes, inherited Session capability,
isolated controller/renderer contracts, submission ownership after waiter drop,
bounded admission, retirement drain and acknowledged replay cursors. Both engines
also check explicit reconciliation cancellation preserves the unknown Message
identity and drains its controller without dispatching a replacement request.
Both engines must finish with zero browser timers and alarms.

The same fixture also runs native `rsi-application` Shell scenarios: ordinary
Profile compilation without a Session consumer, inherited domain capabilities,
independent surface Local identities, dropped open/Surface cleanup, cancellation
during activation and propagation of child cleanup failures through Meta.

The independent UI addon scenarios run in both native Meta and these Workers.
They contribute surfaces, actions and block renderers, and verify declaration
reorder, exact target mappings, stale references, dropped waiters, bounded action
admission, withdrawal drain and failed activation rollback.

The public Session Files client scenarios reject foreign request echoes, wrong
file offsets/lengths/bytes, directory path/name mismatches and non-progressing
pages without replay. They exercise the real shared client with a deterministic
API peer, separately from native filesystem and HTTP authentication tests.

The Session service and renderer are deterministic Rust fixtures. This establishes
portable controller and plugin ownership, not product Web rendering, HTTP domain
integration or a live provider result. CI and application-foundation work consume
this evidence. The [shared runner](../../tools/README.md) verifies the browser
production closure excludes native backends and terminal dependencies.

Run `npm ci --ignore-scripts`, install Chromium/Firefox through Playwright, then
`npm test`. The Rust target is `wasm32-unknown-unknown`; wasm-bindgen CLI must be
0.2.127. `RSI_WASM_BINDGEN` selects an explicit matching executable. This fixture
owns its Cargo and npm lockfiles and never reads user state or credentials.
