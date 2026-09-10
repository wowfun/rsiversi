# rsi-media-api

The [Media contract](../protocol/README.md) owns the shared domain types and semantics.

Ordinary endpoint and client plugins expose `media/import/1` and `media/read/1`.
Import accepts a bounded raw source body of at most 64 MiB; the Media service
owns raster decoding, canonicalization, digest calculation and publication.
Read accepts an exact canonical `MediaRef` and returns that metadata plus the
separate PNG bytes. Client reads revalidate metadata, length and SHA-256 before
exposing a body, preserving its receive lease through clones and slices.

Both operations use the Data lane. Read reserves at most 32 MiB plus 1 KiB for
the response before provider work. Its separate 64 MiB scratch pool admits the
provider's canonical object and bounded envelope before reading. The local
backend retains one file allocation; response encoding copies into its separately
reserved wire allocation. Import transfers the admitted source allocation into
Media without copying it. Codec working-memory admission belongs to Media.

Success means immutable publication. A lost import reply is OutcomeUnknown;
the client does not automatically replay. A later Session failure leaves the
published object available for exact-reference reuse. Uploads are not staged,
and this package exposes no deletion or garbage-collection authority.

Tests use generated images and an isolated local CAS, plus malformed-response
and reply-loss fixtures. Native codecs are excluded from the client build.
