# rsi-meta-plugin-testkit

`rsi-meta-plugin-testkit` drives one trusted plugin through the public C ABI without starting a composition host. Maintained plugins and native fixtures use it for black-box ABI, panic containment, lane backpressure, lifecycle acknowledgement, and invalid-frame behavior.

`PluginHarness` supplies a host table, captures posted control and DATA frames, scripts callback outcomes, delivers lifecycle and service input, and destroys the instance through the ABI. It decodes frames through the canonical [`rsi-meta-plugin`](../../../crates/rsi-meta/plugin/README.md) contract, keeping tests independent from private host types.
