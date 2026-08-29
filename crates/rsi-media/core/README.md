# rsi-media

This ordinary plugin owns raster admission and canonicalization. It bounds
source bytes and decoded pixels, decodes the first frame, converts to RGBA8,
encodes a metadata-free PNG, computes SHA-256 over those final bytes, and asks
the injected backend to publish before returning a reference. Canonical image
bytes must fit the same 32 MiB bound as `MediaRef` and AI image descriptors;
an oversized canonical body is rejected before backend publication.

Codec work runs on Tokio's blocking pool. Cancellation may stop waiting but
cannot preempt an already-running safe-Rust codec call; no durable publication
occurs until canonicalization completes. A configured generation-local
admission ceiling is acquired before a codec task is created, and its permit is
owned by that task until the codec body actually settles even if the requesting
future is dropped.
