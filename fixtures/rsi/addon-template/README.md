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


RSI dependencies share the linked template's immutable published Git revision;
generated manifests never point into the generator checkout. The generated
workspace owns its lockfile. Distribution acceptance builds against that published
SDK, relocates the project and rebuilds offline with the populated cache before
loading Describe/Execute in the current Host. Current-tree SDK acceptance uses
explicit temporary dependency overrides and is reported separately; it does not
claim the uncommitted SDK is remotely available.

The product scaffold test defaults to explicit temporary current-tree SDK
overrides, regenerated offline. Set `RSI_SCAFFOLD_PUBLISHED_SDK=1` only for the
opt-in distribution proof after fetching the pinned dependencies. That branch
preserves the original generated manifest and lock and uses a separate target.
The Linux standard-product CI job fetches the committed template dependencies with
`cargo fetch --locked --manifest-path fixtures/rsi/addon-template/Cargo.toml`, then
runs `RSI_SCAFFOLD_PUBLISHED_SDK=1 cargo test --locked -p rsi --test addons
scaffold::generated_external_locked_workspace_loads_describes_and_executes -- --exact`.
Both branches relocate and perform a second locked offline build before loading.
