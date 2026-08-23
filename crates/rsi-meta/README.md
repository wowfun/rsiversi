# rsi-meta

`rsi-meta` is a process-local foundation for applications assembled from plugins. Its composition model is deliberately small: a `Runtime` owns resources, immutable `Context` values carry scope, and every `Context::apply` creates a separately managed `Fiber`.

The [core crate](core/README.md) owns convergence, lifecycle, services, events, isolation, effects, call tracing, and generation fencing. It knows nothing about files, package manifests, daemons, products, or native libraries. The [native ABI](plugin/README.md) is a narrow byte-call contract. The [Loader](loader/README.md) adapts verified native artifacts into `PluginFactory` values and is itself applied as an ordinary plugin that provides `rsi.meta.loader`.

There is no privileged global composition host, persistence layer, CLI, watcher, or HMR subsystem. Products add those policies as plugins or callers. Native code shares the host process and is trusted; read the [security boundary](docs/security.md).

The current execution and ownership model is defined in [architecture](docs/architecture.md). Use the [testing policy](docs/testing.md) for evidence requirements.
