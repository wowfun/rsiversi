---
name: Reuse prepared model facts and keep media resolution behind its context
---

## Problem

Language Prepare describes the same model up to three times and traverses its
media tree again after accounting for unique bytes. The provider context also
exposes its raw MediaResolver, allowing an adapter to skip the context's byte
admission and body verification even though current adapters use resolve_media.

## Decision

Each direct or deferred Prepare uses one profile for defaults, output bounds
and the frozen snapshot. Positive unique-media weight establishes the need for
a Media reader because valid descriptors have positive byte lengths. Prepare
still rejects unsupported or over-budget requests before resolving dependencies.

The context exposes only admitted, verified media resolution. Its raw-resolver
accessor is removed from the pre-release SDK. Resolver implementations remain an
authoring seam supplied at context construction.

## Alternatives considered

Keeping an unused raw accessor relies on each provider author avoiding a path
that bypasses the context's guarantees. Removing the context-construction bound
instead would weaken direct SDK use; removing the router's early bound would
allow effect-dependency access before rejecting oversized requests. Both checks
therefore remain at their respective boundaries.

## Consequences

Public adapter callbacks perform fewer repeated queries and all context-provided
media bodies follow the same admission and checksum path. This closes an SDK
escape hatch, not a Host-process sandbox: provider plugins remain trusted code.
No shipped adapter was observed using the removed accessor, and no production
corruption or out-of-memory incident is claimed.
