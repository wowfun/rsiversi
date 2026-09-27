# rsi-build-info

A paired build embeds the SHA-256 of the producer's bounded, verified frozen
source manifest. Native clients, Service Host compatibility and the Web Worker
consume the same identity. A standalone Cargo build has no family; native Host
compatibility keeps its executable-digest fallback, while product Web entry
rejects missing identity. This detects incompatible or damaged local artifacts;
it is not a signature or a claim of trust in an artifact distributor.

Native processes launched from a managed publication retain its generation lock
through shutdown, including daemon mode. Garbage collection can remove only
unlocked, inactive publications. External output directories are caller-owned.
