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
The integration provider selects each response script by its exact latest user
input, so child and completion Turns cannot consume another Session's responses.
A deterministic interleaving check exercises that fixture boundary.
The deterministic provider deliberately uses Chat Completions; this does not
change the standard product's Responses default or establish live model behavior.

The `workbench-addon` Cargo example is a minimal embedder of the same declaration
through `standard_application_host`. It accepts explicit Application Profiles,
uses an inert credential store, and exposes no custom product launcher branches.
Build it with `cargo build --locked -p rsi-cli --example workbench-addon`. Freeze that
executable before starting any clients. Set `RSI_WORKBENCH_BINARY` to that absolute
path and `RSI_TUI_PTY_REPORT` to an evidence directory, then run
`cargo test --locked -p rsi-cli --test service_host_cli independent_addon_tui -- --ignored`.
The actual TUI and headless application factories share the same addon, command,
projection, source generation and deterministic provider contract.

With a successful current paired publication from the
[product browser fixture](../web-product/README.md), run
`python3 fixtures/rsi/addon-workbench/run-paired.py /absolute/new-report`.
The browser CI job runs this probe against its current paired publication.
It builds the example from that exact frozen source under the shared build lock,
checks the capture before and after compilation, and retains the publication pin
while its browser clients run. Its separate receipt records the example hash and
family; ordinary Cargo examples cannot be combined with product Web assets.
It uses real Chromium and Firefox, authenticated HTTP and the product Worker,
records the actual provider's plan state and addon Tool declaration, exercises
the addon Settings editor, verifies typed input survives the explicit JSON-mode
switch, and captures the generic extension UI. Device writes
to this source-owned addon namespace are rejected by the product's closed remote
configuration policy; the browser fixture checks that rejection and retained
editor input. Trusted Local Settings mutation remains covered by the public
composition and TUI/headless acceptance targets. Its fixture
configuration hook only writes isolated test inputs before the service starts.

The echo Tool also declares a typed output contract. Browser acceptance opens its
recorded result using the durable baseline and verifies the generic fields and
literal Unicode/HTML text at desktop and narrow widths. The ignored
`independent_addon_tui_renders_saved_typed_result_and_literal_unicode` target
exercises the same card in the actual PTY and retains terminal captures.
