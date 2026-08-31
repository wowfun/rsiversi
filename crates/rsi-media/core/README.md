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

Admission has two byte dimensions in addition to call count. Encoded sources
use a non-queuing 256 MiB generation gate so waiting callers cannot make the
service retain an unbounded source backlog. Temporary source-byte contention is
reported as `AdmissionFull`, distinct from permanent invalid caller input.
After a bounded header probe, codec
work reserves `max(2 * decoder_bytes, decoder_bytes + pixels * 4)` bytes from a
1.6 GB weighted gate. The latter is the current-feature upper bound for one
100-million-pixel 16-bit decode; the separately bounded 32 MiB PNG writer and
encoded-source gate make the conservative source-visible ceiling
1,868,435,456 bytes. These bounds do not claim to measure codec-private memory.
A single image whose computed working weight exceeds the configured generation
gate is rejected before semaphore acquisition; it can never wait for an
impossible number of permits.
Configuration requires the per-image input ceiling to fit within the
generation's encoded-source gate, so every otherwise valid single source has a
representable admission weight.
