# rsi-meta-plugin-fs-watch-polling

`rsi-meta-plugin-fs-watch-polling` is a trusted `fs.watch` provider whose progress is driven explicitly rather than by wall-clock notification timing. It is suitable for deterministic hosts and fixtures. [`plugin.toml`](plugin.toml) owns its contracts and targets, and [`config.schema.json`](config.schema.json) owns its instance input.

## Controlled progress

The plugin requires `fs.read` and injects `runtime.tick`; each tick polls the configured filesystem state and advances pending delivery. Polling state belongs to one generation: prepare creates private state, commit enables delivery, and retirement prevents future work before acknowledgement.

When content hashing is enabled, one watched regular file is limited to 256 MiB and growth is checked while streaming the digest. Metadata-only polling does not apply that content-read limit.

A `Retired` terminal refused by the bounded control lane stays pending for a later tick. Frame backpressure and permanent stream termination follow the [protocol reference](../../../crates/rsi-meta/docs/subsystems/protocols.md). As trusted native code, the plugin inherits the product [security boundary](../../../crates/rsi-meta/docs/security.md).
