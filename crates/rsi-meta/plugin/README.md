# rsi-meta-plugin

`rsi-meta-plugin` defines the fixed-layout v1 C ABI and a safe Rust authoring surface for trusted native plugins. The maintained header is [`include/rsi_meta_plugin.h`](include/rsi_meta_plugin.h).

The host resolves only `rsi_meta_plugin_entry_v1`. A validated plugin table can describe one factory, normalize configuration, create an instance, handle a byte request for a declared service, and destroy instances and the factory. A per-call host table lets native code invoke services it declared as requirements. Context, Fiber lifecycle, routing, events, generation fencing, and cleanup never cross the ABI.

Major versions must match. A host accepts a plugin minor version no newer than
its own; a plugin accepts a host minor version at least as new as the plugin's
required minor. Tables grow only by appending fields. Compatibility checks the
minimum prefix associated with the table's declared minor, not the newest
reader's complete table size. The host zero-initializes its full plugin-table
output before entry so an older plugin leaves every unknown suffix absent.

Buffers always travel as pointer, length, and capacity plus an allocator-matched release callback. A null pointer is valid only for a zero-length input; release callbacks must accept the empty `{NULL, 0, 0}` value. Input borrows and the host table last only for the synchronous callback and must not be retained. Factory callbacks are serialized, calls are serialized per instance but may overlap across instances, and callbacks may run on arbitrary host threads. A plugin must not synchronously create a service-call cycle that re-enters the same instance. On every `create` return, a non-null instance output transfers ownership to the host, including failure returns. A plugin must not unwind across C, call its destruction twice, or return while work still uses an instance being destroyed. The SDK trampolines contain factory construction, callback, and destructor panics; they cannot contain aborts, memory faults, or arbitrary native behavior.

Implement `NativePlugin` and `NativeInstance`, then invoke `export_plugin!(YourPlugin)`. The descriptor is JSON matching `rsi_meta::PluginDescriptor`; the Loader replaces its self-reported identity with the verified artifact hash.
