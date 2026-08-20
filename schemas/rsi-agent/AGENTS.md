- These schemas own the exact version-zero external JSON shape and the
  per-field character and collection bounds expressible in JSON Schema. The
  protocol decoder owns aggregate encoded-byte and recursive JSON-complexity
  limits. Read the product [README](../../crates/rsi-agent/README.md) for
  semantics.
- Keep every envelope closed and its expressible fields bounded. Update the
  [`rsi-agent-protocol`](../../crates/rsi-agent/protocol/) DTO validation and
  contract tests with every field, bound, or wire-version change.
- Preserve the `https://rsi-agent.invalid/...` logical schema ids across
  physical path-only changes.
