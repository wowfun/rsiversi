# rsi-build-info

The build script verifies the producer's bounded frozen source manifest and
embeds whether the publication is managed. The native generation lease consumes
that flag. Publication receipts and the Web Worker's internal bundle identity
belong to [application distribution](../../../apps/devtools/README.md); this
library exposes no build-family digest or native compatibility policy. Source
verification detects changed inputs, not trust in an artifact distributor.

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
