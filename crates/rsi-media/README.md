# rsi-media

`rsi-media` owns durable content-addressed image references. The
[`rsi-media-protocol`](protocol/README.md) package defines immutable refs and
backend/service contracts. The ordinary [`rsi-media`](core/README.md) plugin
decodes bounded raster inputs, discards source metadata, converts the first
frame to RGBA8, encodes canonical PNG bytes, and publishes only after its
backend commits. Canonical encoding writes through the same 32 MiB output
bound, so rejection does not first allocate an unbounded encoded PNG.
[`rsi-media-local`](local/README.md) provides a local immutable
CAS; [`rsi-media-testkit`](testkit/README.md) provides memory storage.

The public ref deliberately carries no normalizer version. Any change to final
bytes produces a new identity. Audio, video, arbitrary files, URL fetches, CLI
export, and garbage collection are outside the current contract.
