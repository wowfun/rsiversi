Read [the product architecture](docs/architecture.md), [security boundary](docs/security.md), and [testing policy](docs/testing.md) before changing `rsi-meta` behavior.

- Keep `Runtime -> Context -> Fiber` as the sole composition model. Discovery and execution backends implement `PluginFactory`; they do not enter core.
- Keep unsafe code out of `core`. `plugin` and `loader` are the deliberate ABI/loading exceptions; document every unsafe operation contract there.
- Test core semantics through public Context and Fiber behavior. Native tests must cross a real dynamic-library boundary on the host platform.
