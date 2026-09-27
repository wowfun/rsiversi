# rsi-meta architecture

The foundation has one composition graph and one lifetime graph:

```text
Runtime
  └─ persistent Context
       └─ apply ResolvedFactory
            └─ Fiber generation
                 ├─ prepared Local and Portable requirements
                 ├─ dynamic Local and Portable supplies
                 ├─ listeners and child Fibers
                 ├─ transferable capabilities
                 └─ reverse-ordered effects
```

No product registry, factory catalog, Profile loader, native branch, daemon, or
persistence mechanism is embedded in core. `PluginFactory` contains execution
behavior; callers resolve immutable identity and update policy before applying
it. A product authority is a plugin Fiber; a passive leaf value is an
effect-owned contribution to that authority.

[Runtime lifecycle](subsystems/runtime-lifecycle.md) defines cancellation and
reverse teardown across that lifetime graph. [Supplies and capabilities](subsystems/supplies-and-capabilities.md)
defines how published values and transferable authority participate in it. The
[native adapter](subsystems/native-adapter.md) preserves those contracts across
library boundaries without making native code a separate composition system.
