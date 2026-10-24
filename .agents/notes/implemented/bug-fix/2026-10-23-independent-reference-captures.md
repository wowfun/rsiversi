---
name: Independent pending reference captures
---

## Problem

Content-addressed snapshots can be identical for two open previews. Giving those
previews one shared releasable lease lets the first close remove the second's CAS
root. A counter alone makes duplicate release indistinguishable from the second
preview closing.

## Decision

Every successful capture has its own opaque capture identity and pending lease.
Explicit release consumes that identity idempotently within its target Session;
content hashes still deduplicate CAS bytes and verified envelope decoding.
The [reference owner](../../../../crates/rsi-agent/references/README.md) defines
retention and acceptance. Session format 23, Session and History API revisions 5,
and composer journal schema 5 carry the identity. Old unsupported formats are
refused without rewriting durable state.

## Alternatives considered

A content-keyed reference count cannot associate retries with their original
owner. A timer makes a live preview depend on elapsed time. Duplicating CAS content
would lose deduplication without establishing independent release ownership.

## Consequences

Each capture consumes its own bounded slot, including duplicate content. Existing
in-process verification continues to cache by content identity. Tests capture the
same SQLite-backed bytes twice, release one twice, attempt GC, verify the second,
and then release it. UI cleanup compares capture identity, while draft presentation
can still suppress duplicate content.
