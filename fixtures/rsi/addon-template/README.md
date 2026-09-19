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
