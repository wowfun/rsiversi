# rsi-agent

`rsi-agent` is a durable, replayable agent-turn runtime. It records a bounded user-to-model-to-tool loop as an append-only transcript and reaches providers only through an [`rsi-meta`](../rsi-meta/README.md) composition.

The runtime currently requires Unix workspace-locking semantics and supports Linux and macOS. Non-Unix targets are rejected at compile time rather than running with a weaker lease.

The v0 product executes one turn per caller-supplied session identifier. Repeating a completed request with the same identifier, exact model, and prompt returns its stored outcome without calling a provider again; changing the model or prompt is a conflict. Replay means reconstructing model requests and reading recorded outcomes, never re-executing an external effect.

## Components

| Component | Responsibility |
|---|---|
| [`rsi-agent`](core/README.md) | `AgentHost`, typed AI operations, public session/transcript values, artifact CAS, SQLite durability, and recovery |
| [`rsi-agent-protocol`](protocol/README.md) | Tool-service messages, aggregate validation, and canonical encoding |

The closed JSON service shapes and per-field bounds live in [`schemas/rsi-agent`](../../schemas/rsi-agent/); aggregate encoded-byte and JSON-complexity bounds are enforced by protocol decoding. Deterministic native providers and the assembled black-box scenario live in [`fixtures/rsi-agent`](../../fixtures/rsi-agent/).

## Product boundary

`AgentHost` is the only runtime façade. Callers provide an agent workspace, an already-open `CompositionHost`, and a consumer instance whose graph binds `rsi.ai.language`, the optional media/Realtime services it uses, and `rsi.agent.tools`. `AgentHost` owns its transcript database and artifact CAS but does not own or close the composition host. Language requests select only a model; composition chooses the provider instance and plugin config fixes endpoint and protocol.

SQLite, request derivation, stream adapters, recovery, admission coordination, and per-session execution are private implementation details. A cloneable host admits unrelated sessions concurrently while keeping each turn and its tool calls ordered. Model providers do not define transcript syntax, and tool providers do not decide durable dispatch semantics. There is no public persistence, model, or tool trait.

`OpenOptions` configures the nonzero concurrent-session limit and a host-wide `ExecutionLimits` policy. The latter bounds handshakes, model responses, tool responses, and the provider-facing portion of a turn; durable failure closure is still allowed to complete after a provider deadline so timeout handling cannot strand an open transcript.

Image generation, transcription, and speech are independent typed operations. Realtime is a live non-replayable session. Each direct operation has a caller-owned `AiOperationId` and durable Reserved/Prepared/Started/terminal barriers; reservation occurs before provider work or input-media reads. Image and audio bytes are committed as digest-addressed artifacts. The current release has no user interface, multi-turn continuation, public language-stream API, cancellation API, parallel tool execution, compaction, workflow engine, self-modification, or shell/file tool.

## Documentation

- [Architecture](docs/architecture.md) defines the execution, transcript, replay, recovery, and resource-bound contracts.
- [Security](docs/security.md) defines native-provider trust, durable-state ownership, and failure containment.
- [Testing](docs/testing.md) defines invariant evidence and the keyless conformance scenario.
- The repository [architecture](../../docs/architecture.md) defines the boundary between this product and `rsi-meta`.
