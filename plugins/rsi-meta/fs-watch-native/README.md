# rsi-meta-plugin-fs-watch-native

`rsi-meta-plugin-fs-watch-native` is a trusted native provider of `fs.watch` for hosts that want operating-system filesystem notifications. [`plugin.toml`](plugin.toml) owns its provided and required contracts and supported targets; [`config.schema.json`](config.schema.json) owns its instance input.

The plugin requires `fs.read` and injects `runtime.tick`. Native notifications detect changes, while ticks retry DATA frames and terminal lifecycle frames that were refused by a bounded host mailbox.

## Notification lifetime

Prepare creates generation-private watcher state without publishing a route. Commit admits notifications from that generation, and retirement stops its watcher work before acknowledgement. A `Retired` terminal that meets control-lane backpressure remains pending until a later tick can deliver it.

Queue pressure follows the independently bounded control and DATA lanes in the [protocol reference](../../../crates/rsi-meta/docs/subsystems/protocols.md). The plugin is trusted native code and inherits the product [security boundary](../../../crates/rsi-meta/docs/security.md).
