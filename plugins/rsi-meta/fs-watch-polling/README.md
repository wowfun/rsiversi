# rsi-meta-plugin-fs-watch-polling

`rsi-meta-plugin-fs-watch-polling` is a trusted `fs.watch` provider whose progress is driven explicitly rather than by wall-clock notification timing. It is suitable for deterministic hosts and fixtures. [`plugin.toml`](plugin.toml) owns its contracts and targets, and [`config.schema.json`](config.schema.json) owns its instance input.

## Controlled progress

The plugin requires `fs.read` and injects `runtime.tick`; each tick polls the configured filesystem state and advances pending delivery. Polling state belongs to one generation: prepare creates private state, commit enables delivery, and retirement prevents future work before acknowledgement.

Polling is metadata-only by default. Content-hash mode rereads every regular file so equal-length rewrites with preserved or coarse timestamps cannot disappear behind metadata equality. Hashing runs on one bounded worker, is capped at 256 MiB across one tick, prioritizes previously deferred streams on the next tick, and checks for shutdown between 64 KiB chunks. One watched regular file is limited to 256 MiB. Metadata-only polling does not apply that content-read limit.

A `Retired` terminal refused by the bounded control lane stays pending for a later tick. Frame backpressure and permanent stream termination follow the [protocol reference](../../../crates/rsi-meta/docs/subsystems/protocols.md). As trusted native code, the plugin inherits the product [security boundary](../../../crates/rsi-meta/docs/security.md).
