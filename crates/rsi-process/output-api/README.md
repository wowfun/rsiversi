# rsi-process-output-api

The [Process contract](../core/README.md) owns the shared domain types and semantics.

Ordinary endpoint and client plugins expose `output/read/1` using only the
read-only Process output-cache contract. Requests carry an output identity,
raw offset and 1–65,536 byte limit; responses carry closed cursor metadata
and a separate binary payload. The client validates identity, offset, exact
length, total bound and forward progress before exposing a page.

The Data lane reserves at most 65 KiB per response. A separate 1 MiB endpoint
budget admits the provider page before reading, covering the bounded temporary
page while copying into the reserved response. Client pages retain their wire
allocation through byte clones and slices. Missing/evicted output remains an
unavailable cache result, never an empty stream or Session archive.

Tests exercise real local output capture, arbitrary bytes and cursors through
the public API, malformed metadata, lease retention and plugin withdrawal.
