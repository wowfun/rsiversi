---
name: Deterministic repository text line endings
---

## Problem

Platform checkout conversion changes bytes used by `SHIPPED_PRESETS` in product
composition and by compiled asset identities. Equal source revisions must not
gain different byte digests merely because a contributor checked out on Windows.

## Decision

The root `.gitattributes` keeps Git text detection and uses LF for detected text
in every working tree. PNG and ICO remain explicitly binary. This is repository
governance; owning products still define their byte and asset identity contracts.

## Alternatives considered

Relying on contributor `core.autocrlf` settings makes byte identity ambient.
Normalizing only shell scripts leaves compiled text assets exposed. Rewriting
every tracked blob is unnecessary because the existing blobs already use LF.

## Consequences

New checkouts have deterministic text bytes. Existing CRLF working-tree files
normalize cleanly through Git without a bulk renormalization change. A future
file format that requires different line endings must declare that exception
explicitly rather than depending on host settings. This change does not alter
binary assets or grant platform runtime support.
