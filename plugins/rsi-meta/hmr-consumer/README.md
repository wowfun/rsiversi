# rsi-meta-plugin-hmr-consumer

`rsi-meta-plugin-hmr-consumer` provides `hmr.watch-consumer` for development assemblies. It consumes `fs.watch` and asks the host control capability to apply an installed manifest and lock pair; the registry, not this plugin, owns resolution, durable installation, graph cutover, and restart behavior.

## Configuration and dependencies

[`plugin.toml`](plugin.toml) declares the `control.apply-manifest` and `fs.read` requirements, the `fs.watch` and `runtime.tick` injections, supported targets, and `process_fixed = true`. [`config.schema.json`](config.schema.json) validates the manifest path, lock path, and watch request id.

## Generation-scoped application

The plugin emits content-derived apply requests only for the generation that admitted them and only for its configured manifest and lock pair. It keeps one request in flight: applied or unchanged feedback clears matching drift, deterministic rejection or `restart_required` suppresses retries for that content, and transient failure leaves it dirty for a later tick. A `Retired` terminal refused by bounded control-lane backpressure stays pending until `runtime.tick` can deliver it. The [composition runtime](../../../crates/rsi-meta/docs/subsystems/composition-runtime.md) owns candidate, cutover, and restart semantics.

Watch-plan inputs are bounded to 256 MiB per file and 512 MiB in aggregate. One bounded background worker derives and hashes a plan; at most one refresh is in flight, and further notifications collapse into one dirty retry. A Prepare callback only admits that bounded work: its matching `Prepared` or `PrepareFailed` terminal is emitted asynchronously and retried under control-lane backpressure. Shutdown cancellation is checked between bounded reads.

This package is trusted native code and inherits the product [security boundary](../../../crates/rsi-meta/docs/security.md).
