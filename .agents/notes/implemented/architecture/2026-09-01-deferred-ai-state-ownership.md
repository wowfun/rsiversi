---
name: Protocol-owned shared deferred AI state
comment: Keep one typed checkpoint wire while avoiding per-delta reconstruction
---

## Problem

Protocol and provider packages carried parallel deferred status, checkpoint,
and batch representations. Every provider batch converted between them and
revalidated the complete frozen call snapshot. OpenAI also reconstructed and
serialized its unchanged parser extension for every ordinary text delta.

## Decision

`rsi-ai-protocol` exclusively owns deferred status, checkpoint, and batch
types; the provider SDK directly re-exports them. `ProviderExtension` is a
closed construction- and decode-validated value backed by shared immutable
state with an exact cached encoded length. Deferred checkpoint clones share
the immutable call and operation identity, and advance validates the new
status, sequence, terminal relation, and provider extension without
revalidating frozen identity.

The OpenAI Responses parser retains the current extension plus a dirty bit. It
rebuilds state only when the open-block map, next index, or tool-seen bit
changes. Batch commit advances a candidate checkpoint and validates the complete
event batch before replacing the shared checkpoint, so a rejected batch cannot
publish a cursor past events that were never delivered.

## Alternatives considered

Caching serialized bytes alongside the provider's parallel checkpoint would
reduce one allocation but preserve two authorities and their conversion
failure surface. Exposing the internal `Arc` would let callers couple to the
sharing mechanism. Omitting decode validation was rejected because checkpoint
JSON is durable input even though in-process clones are trusted.

## Consequences

Provider adapters and the router exchange one typed value and still compare
the prepared snapshot item by item at restore. A 10,000-delta unchanged block
shares one provider-state allocation; block-count tests cover 1, 16, and 256
open blocks. Custom serializers preserve the exact existing extension and
six-field checkpoint JSON bytes, so existing durable bytes need no migration.
Construction and decode now apply the extension byte ceiling to the complete
extension envelope, not only its JSON value.
