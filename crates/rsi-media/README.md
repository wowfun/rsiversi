# rsi-media

`rsi-media` owns durable content-addressed image references. The
[`rsi-media-protocol`](protocol/README.md) package defines immutable refs and
backend/service contracts. The ordinary [`rsi-media`](core/README.md) plugin
decodes bounded raster inputs, discards source metadata, converts the first
frame to RGBA8, encodes canonical PNG bytes, and publishes only after its
backend commits. Canonical encoding writes through the same 32 MiB output
bound, so rejection does not first allocate an unbounded encoded PNG.
Callers may tighten the canonical byte bound and require a source MIME type
through `ImageImportOptions`. The decoder checks the actual source format against
that declaration, and the bounded encoder enforces the tightened limit before
any backend publication. A result-wide budget can pass its remaining bytes for
each successive import. A failed later import does not delete earlier CAS objects.
[`rsi-media-local`](local/README.md) provides a local immutable
CAS; [`rsi-media-testkit`](testkit/README.md) provides memory storage.

The public ref deliberately carries no normalizer version. Any change to final
bytes produces a new identity. Audio, video, arbitrary files, URL fetches, CLI
export, and garbage collection are outside the current contract.

The ordinary Media API endpoint and client plugins expose independent import
and canonical-reference reads. Remote upload is bounded to a 64 MiB frame,
including import options; the canonical image bound remains 32 MiB.
`media/import/2` carries a two-byte big-endian options length, up to 1024 bytes
of JSON `ImageImportOptions`, followed by the source bytes. Reads remain version 1.
