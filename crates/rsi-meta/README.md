# rsi-meta

`rsi-meta` is RSIversi's composition foundation. Its one ownership model is
`Runtime -> Context -> Fiber`: every long-lived authority is a plugin
generation, while leaf tools, commands, prompt fragments, and similar values
are effect-owned contributions to product plugins.

The [core crate](core/README.md) owns bounded lifecycle, dynamic injection and
supply, transactional effects, message capabilities, events, and shutdown. It
contains no unsafe code and knows nothing about files, product schemas, or
native libraries. The independent `rsi-meta-scope` crate supplies safe Rust
scope identity and layered contribution storage without adding a generic
Runtime registry.

The [native ABI](plugin/README.md) expresses the same capability and effect
model through a versioned C boundary. The [Loader](loader/README.md) validates
and maps trusted native artifacts, then adapts them into ordinary
`PluginFactory` values. Native loading is not privileged core behavior.

Current contracts live in the product [architecture](docs/architecture.md),
[security boundary](docs/security.md), and [testing policy](docs/testing.md).
The rationale and cutover criteria are recorded in the
[Cordis capability foundation Agent Note](../../.agents/notes/implemented/architecture/2026-08-24-cordis-capability-foundation.md).
