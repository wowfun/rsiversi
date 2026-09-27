# Native adaptation

## Native adapter

The native path remains an adapter chain:

```text
explicit path -> verified staged mapping -> ABI v3 Portable capability port
              -> ResolvedFactory -> ordinary Fiber
```

ABI v3 carries bounded Messages, capability ownership, exact Portable contract
metadata, dynamic provide, and setup effects across one exchange port. The
native loader maps foreign handles into the same core authorities used by
Portable Rust adapters; it does not introduce a second declaration registry or
lifecycle model. Callback-bound channel and effect authority cannot become
durable product state. The maintained
[`rsi_meta_plugin.h`](../../native/include/rsi_meta_plugin.h) owns the exact entry,
version, frame, opcode, status, and one-shot release contract.

Native code is trusted process code selected by explicit artifact path, not a
package or sandbox. Host computes and records the exact staged-byte digest on
start or requested reload; the artifact path is not watched. A create or call timeout
terminalizes its Runtime, but the callback frame, thread, mapped library,
capabilities, cache lease, and accounting remain retained until foreign code
actually returns. Loader admission is fail-fast and preserves callback lineage
across nested calls so native callbacks cannot turn serialization into a
deadlock. The [Loader contract](../../native-loader/README.md) owns cache, finalization,
and platform details.
