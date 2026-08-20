- These schemas own the exact version-zero provider-neutral request shapes for
  [`rsi-ai`](../../crates/rsi-ai/README.md). Runtime decoders additionally own
  aggregate byte, recursive JSON-complexity, stream-grammar, and binary bounds.
- Keep request objects closed and media locator-free. Update DTO/schema
  conformance tests with every serialized field or bound change.
- Preserve the `https://rsi-ai.invalid/v0/...` logical ids unless the protocol
  identity itself changes.
