Read the [product contract](README.md), [architecture](docs/architecture.md),
[security boundary](docs/security.md), and [testing policy](docs/testing.md)
before changing `rsi-ai` behavior.

- `rsi-ai-protocol` owns provider-neutral semantic and wire contracts. Keep
  provider JSON, HTTP, SSE, and WebSocket syntax in concrete provider crates.
- Keep the caller interface deep: callers use the Registry and typed
  capability handles; provider authors implement capability-specific adapter
  interfaces; transport adapters remain internal.
- A prepared call is one-shot and freezes validated request, model, provider
  configuration, credential source, and media digests before external I/O.
- Every external or durable input is closed and bounded at its owning seam.
  Secret values, raw provider bodies, and binary media never enter Debug output
  or JSON extension envelopes.
- Default tests are keyless and use scripted adapters or local mock servers.
  Live provider tests must be explicit opt-ins.

