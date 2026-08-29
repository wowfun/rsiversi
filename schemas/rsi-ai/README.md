# rsi-ai schemas

These Draft 2020-12 schemas define the Language and Image request shapes.
They express field-level JSON constraints; [`rsi-ai-protocol`](../../crates/rsi-ai/protocol/README.md)
remains authoritative for aggregate byte limits, recursive JSON limits, role
grammar, media-kind relationships, and stream state machines.

JSON Schema `maxLength` counts Unicode scalar values. Where the Rust contract
names a UTF-8 byte limit, the schema is an interoperable early screen and the
validating protocol decoder is authoritative; a multibyte string can therefore
pass the schema ceiling and still be rejected at the typed boundary.
