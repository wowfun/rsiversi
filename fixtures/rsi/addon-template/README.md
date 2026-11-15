# rsi-addon-template

This standalone safe-SDK cdylib is the maintained input to `cargo xtask addon new`.
It derives the Portable Tool Describe/Execute exchange from the broader
[native addon fixture](../native-addon/README.md), retaining only one echo tool.
It deliberately has no provider, UI, API, confinement or finalizer probes. The
original fixture continues to own evidence for those independent boundaries.

The repository-tool tests generate this workspace outside the checkout, preserve
its locked dependency graph, and exercise destination publication and argument
validation. The standard product's native addon scenario loads the generated
library through the real Loader and calls the tool through an Agent pin.

The [generator contract](../../../crates/tools/rsi-xtask/README.md) defines the local
SDK binding and checkout lifetime. This fixture uses relative workspace paths;
generation resolves them into absolute paths to the invoking checkout and copies
the fixture lockfile, changing only its root package name.

The Linux standard-product CI job runs
`cargo test --locked -p rsi-app-catalog --test addons
scaffold::generated_external_locked_workspace_loads_describes_and_executes -- --exact`.
The test builds the generated manifest and lock without alteration, relocates the
project, rebuilds offline with cached dependencies, then loads Describe/Execute in
the current Host. This proves local SDK integration, not public distribution.
