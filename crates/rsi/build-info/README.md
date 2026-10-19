# rsi-build-info

A paired build embeds the SHA-256 of the producer's bounded, verified frozen
source manifest. Native clients, Service Host compatibility and the Web Worker
consume the same identity. A standalone Cargo build has no family; native Host
compatibility keeps its executable-digest fallback, while product Web entry
rejects missing identity. This detects incompatible or damaged local artifacts;
it is not a signature or a claim of trust in an artifact distributor.

Native processes launched from a managed publication retain its generation lock
through shutdown, including daemon mode. Garbage collection can remove only
unlocked, inactive publications. The compiled manifest requires an explicit boolean
`managed` field; missing or mistyped flags reject the build before identity is emitted.
Its lease lives at `target/rsi-app/<target>/leases/<generation-id>`, outside the directory
that GC renames; acquisition is nonblocking and missing managed leases fail closed.
After acquiring the shared lock, the client rechecks that the managed marker
still exists. An opened lease handle alone does not pin a generation: collection
may finish before that handle acquires its lock. Shell selection can therefore
race collection before startup; rerunning the entrypoint selects current again.
An adjacent `.rsi-generation.lock` marker identifies managed watcher inputs.
External output directories are caller-owned.
