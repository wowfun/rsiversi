# Independent workbench addon acceptance

This keyless fixture is compiled into the product integration tests through its
own source module. It uses only public composition, plugin, Settings and Tool
interfaces. One `StandardAddon` declares an Agent Tool, the existing independent
PlanPolicy factory under an addon-owned factory name, and a Service Settings
consumer. Its explicit Agent Profile selects those contributions; generic Session
projection and Settings presentation consume their data without addon-specific
branches in the product. Configuration and callback code stay with the fixture.

The acceptance target checks draft state before first atomic publication, durable
context and Tool denial, Profile generation changes, resident pins, fork boundary
state, cold recovery and removal. Settings writes exercise the ordinary validator
and registration lifetime. Actual renderers and native Loader retention require
separate evidence; an in-process projection assertion is not visual evidence.

Run `cargo test --locked -p rsi --test session_service addon_acceptance`.
The deterministic provider deliberately uses Chat Completions; this does not
change the standard product's Responses default or establish live model behavior.

The `workbench-addon` Cargo example is a minimal embedder of the same declaration
through `standard_application_host`. It accepts explicit Application Profiles,
uses an inert credential store, and exposes no custom product launcher branches.
Build it with `cargo build --locked -p rsi --example workbench-addon`. Freeze that
executable before starting any clients. Set `RSI_WORKBENCH_BINARY` to that absolute
path and `RSI_TUI_PTY_REPORT` to an evidence directory, then run
`cargo test --locked -p rsi --test service_host_cli independent_addon_tui -- --ignored`.
The actual TUI and headless application factories share the same addon, command,
projection, source generation and deterministic provider contract.

With standard Web assets built as described by the
[product browser fixture](../web-product/README.md), set `RSI_WORKBENCH_BINARY`,
`RSI_WEB_ASSETS` and `RSI_WORKBENCH_REPORT`, and run `node fixtures/rsi/addon-workbench/verify.mjs`.
It uses real Chromium and Firefox, authenticated HTTP and the product Worker,
records the actual provider's plan state and addon Tool declaration, exercises
the addon Settings editor, and captures the generic extension UI. Device writes
to this source-owned addon namespace are rejected by the product's closed remote
configuration policy; the browser fixture checks that rejection and retained
editor input. Trusted Local Settings mutation remains covered by the public
composition and TUI/headless acceptance targets. Its fixture
configuration hook only writes isolated test inputs before the service starts.
