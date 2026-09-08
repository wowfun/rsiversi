# rsi-meta-browser-probe

This standalone fixture exercises public Execution and Runtime behavior inside
real Dedicated Workers. The browser harness observes results and Worker errors;
it does not emulate an executor or run native Rust as a substitute. Timer and
Runtime resource snapshots establish cleanup only after owned work completes.

The [Meta testing guide](../../../crates/rsi-meta/docs/testing.md) defines the
interpretation of browser evidence. The probe covers task and preparation waiter
drop, timer cancellation, non-yielding deadlines, Local activation and withdrawal,
rollback order, resource reclamation and an intentional fatal Worker trap.
It also installs two Host-prepared child Profiles in one Runtime, with independent
Local isolation and controls. Disposing either child leaves the other child and
the parent's services active.

Run `npm ci`, `npx playwright install chromium firefox`, then `npm test` here.
The root Rust toolchain needs `wasm32-unknown-unknown`; the wasm-bindgen CLI must
be version 0.2.127. `RSI_WASM_BINDGEN` can name an explicit CLI executable. The
harness builds the locked Rust fixture, generates bindings, and serves an exact
asset allowlist on an ephemeral loopback port. Each browser runs two independent
successful Workers alongside one trapped Worker, which may never acknowledge
cleanup. No credentials or real user state are used.
