# RSIversi monorepo architecture

The repository is a family of independently usable products. Each product owns its public contract, trust boundary, and verification policy; the root owns only one-way dependency direction and workspace governance.

[`rsi-meta`](../crates/rsi-meta/README.md) is the lowest active runtime layer. It owns process-local plugin composition through `Runtime`, `Context`, and Fiber, plus an adapter-neutral `PluginFactory` seam. Native ABI and library mapping live outside core.

[`rsi-ai`](../crates/rsi-ai/README.md) is an independent provider-neutral AI SDK. It owns semantic capability protocols, provider authoring, exact routing, and standalone transports. It has no active dependency on `rsi-meta`; a future integration must be an ordinary plugin rather than a privileged wrapper.

[`rsi-agent-protocol`](../crates/rsi-agent/protocol/README.md) owns the current agent/tool wire contract. No online `rsi-agent` runtime or coding-tools plugin is present: the obsolete implementations were removed rather than retained as a second architectural truth. A future runtime must be built over the public plugin foundation without restoring the removed CompositionHost boundary; historical rationale remains in Agent Notes and Git history.

Product components live at `crates/<product>/<component>`. Non-crate assets use the matching namespace below `schemas/`, `plugins/`, `fixtures/`, or `examples/`. Repository-only tools live below `crates/tools/`. Current behavior is documented at its owning product or package rather than duplicated at root.
