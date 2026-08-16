# rsi-meta

`rsi-meta` is a trusted native plugin composition platform. It resolves scoped service graphs, validates and stages native plugin packages, commits graph changes durably, and exposes the same control model through an embedded Rust facade and local daemon adapters.

Native plugins share the daemon's process, security, and failure domain. Read the [security boundary](docs/security.md) before loading code that is not fully trusted.

## Components

| Component | Responsibility |
|---|---|
| [`rsi-meta`](core/README.md) | Offline `CompositionProject`, online `CompositionHost`, graph lifecycle, durable operations, routing, and service streams |
| [`rsi-meta-cli`](cli/README.md) | CLI plus Unix socket and loopback HTTP/WebSocket adapters |
| [`rsi-meta-loader`](loader/README.md) | Package validation, configuration preparation, artifact staging, and dynamic-library ownership |
| [`rsi-meta-plugin`](plugin/README.md) | Language-neutral C ABI and the safe Rust plugin SDK |

Registry, routing, persistence, recovery, runtime actors, and loader authorities remain private implementation details. Daemon control envelopes belong to the CLI adapter rather than the embedded core.

## Start here

Use the [echo example](../../examples/rsi-meta/echo/README.md) for a complete build, lock, and foreground-daemon path. Native plugin authors should follow the [plugin authoring tutorial](docs/cookbook/plugin-authoring.md).

## Documentation

- [Architecture](docs/architecture.md) maps the product components and execution flow.
- [Composition runtime](docs/subsystems/composition-runtime.md) defines graph, lifecycle, HMR, and durability semantics.
- [Protocols](docs/subsystems/protocols.md) defines commands, events, transports, and service streams.
- [Configuration](docs/subsystems/configuration.md) defines composition, package, lock, and secret ownership.
- [Security](docs/security.md), [development](docs/development.md), and [testing](docs/testing.md) own their respective product contracts.

The v0 product stops at this platform boundary. A future agent product is intentionally layered above it, as described by the repository [architecture](../../docs/architecture.md).
