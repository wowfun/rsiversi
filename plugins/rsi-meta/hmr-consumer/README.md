# rsi-meta-plugin-hmr-consumer

`rsi-meta-plugin-hmr-consumer` provides `hmr.watch-consumer` for development assemblies. It consumes `fs.watch` and asks the host control capability to apply an installed manifest and lock pair; the registry, not this plugin, owns resolution, durable installation, graph cutover, and restart behavior.

## Configuration and dependencies

[`plugin.toml`](plugin.toml) declares the `control.apply-manifest` and `fs.read` requirements, the `fs.watch` and `runtime.tick` injections, supported targets, and `process_fixed = true`. [`config.schema.json`](config.schema.json) validates the manifest path, lock path, and watch request id.

## Generation-scoped application

The plugin emits content-derived apply requests only for the generation that admitted them and only for its configured manifest and lock pair. It keeps one request in flight: applied or unchanged feedback clears matching drift, deterministic rejection or `restart_required` suppresses retries for that content, and transient failure leaves it dirty for a later tick. A `Retired` terminal refused by bounded control-lane backpressure stays pending until `runtime.tick` can deliver it. The [composition runtime](../../../crates/rsi-meta/docs/subsystems/composition-runtime.md) owns candidate, cutover, and restart semantics.

Watch-plan inputs are bounded to 256 MiB per file and 512 MiB in aggregate before the callback hashes their desired bytes.

This package is trusted native code and inherits the product [security boundary](../../../crates/rsi-meta/docs/security.md).
