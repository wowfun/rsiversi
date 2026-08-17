# rsi-meta-plugin

`rsi-meta-plugin` is the ABI and Rust SDK for authors of trusted native `rsi-meta` plugins. The maintained C header and Rust declarations are the exact ABI sources; the [authoring tutorial](../docs/cookbook/plugin-authoring.md) covers package assembly and host integration.

## ABI and SDK

The C boundary contains only fixed-width integers, opaque handles, function pointers, and pointer/length byte pairs. Rust containers, trait objects, allocator ownership, references, and unwinding never cross it. Hosts resolve only `rsi_meta_plugin_entry_v0`. The entry receives the host table, writable plugin-table storage, and that storage's byte capacity; the SDK checks capacity before writing, and both sides validate the advertised table prefix, reserved word, required handles, and ABI version.

The safe SDK exposes the host and plugin interfaces, lane and outcome types, and `export_plugin!`. Its trampolines catch Rust unwinding during construction, dispatch, shutdown, and destruction; they cannot contain aborts, memory faults, or arbitrary native behavior.

## Calls, lanes, and lifetime

Host callbacks for one plugin handle are serialized but may execute on different threads. `host_post_frame` may be called concurrently and copies accepted bytes before returning. Control and DATA lanes are independently bounded: `WouldBlock` rejects only the current attempt, while `Closed` is permanent.

The JSON [plugin-frame schema](../../../schemas/rsi-meta/plugin-frame.schema.json) defines exactly four control bodies: `lifecycle`, `service_request`, `service_event`, and `durable_command`. DATA never uses JSON integer arrays. On the internal plugin lane it uses `RMD0`, followed by a one-byte request/event discriminator, big-endian `u16` request-ID and service lengths, a big-endian `u32` payload length, then the UTF-8 identifiers and raw payload bytes. `Frame::service_data_request` and `Frame::service_data_event` are the canonical constructors. `RMD0` is deliberately distinct from the public transport's [`RSD0`](../docs/subsystems/protocols.md#service-streams): the host translates between plugin request/service correlation and public stream/sequence correlation.

Raw pointer, callback, table-layout, concurrency, and lifetime requirements live on the unsafe declarations in source. Cross-package prepare, commit, retirement, and service-stream behavior belongs to the [composition](../docs/subsystems/composition-runtime.md) and [protocol](../docs/subsystems/protocols.md) references.
