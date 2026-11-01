---
name: Keep completion discovery and response classification at their existing owners
---

## Problem

The product controller exposes separate skill and Agent completion wrappers even
though consumers use its combined catalog. Calling both wrappers repeats Sources
discovery. The finite HTTP head also stores a domain-error flag derived from
status, while transport decoding classifies the original status independently.

## Decision

The controller exposes completion discovery through `completion_catalog`, whose
private helpers share one Sources result. The validated finite response head
contains encoding and length; HTTP status remains the error-classification input.
The unused split wrappers and redundant flag are removed.

## Alternatives considered

Keeping the wrappers preserves an alternate discovery path with no current
consumer. Making transports consume the duplicate flag instead moves an existing
status decision across the metadata boundary without improving validation.
Neither option justifies maintaining the extra surface.

## Consequences

This is a pre-release Rust API removal. Completion contents, per-source bounds,
HTTP media-type checks and typed domain-error behavior stay with their current
owners. This decision does not imply that independent public libraries or
documented extension contracts can be deleted solely for lacking a standard
application caller.
