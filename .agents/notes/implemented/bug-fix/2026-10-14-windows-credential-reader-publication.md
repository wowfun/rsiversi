---
name: Windows credential readers retain snapshots during atomic publication
---

## Problem

Credential readers run concurrently with serialized writers. A private writable
read handle excludes deletion; even a delete-sharing reader prevents ordinary
Windows rename replacement while it remains open.

## Decision

Credential reads use the retained parent's no-follow read-only acquisition and
validate the exact private descriptor and single-link policy on that same handle.
Native file replacement uses extended rename with POSIX semantics, so existing
delete-sharing readers keep the previous object and subsequent readers open the
published object. Writable private handles and namespace pins continue excluding
delete sharing. The [native contract](../../../../crates/rsi-files/native-fs/README.md)
owns publication mechanics; the [credential contract](../../../../crates/rsi-credentials/local/README.md)
owns read consistency and secret validation.

## Alternatives considered

Serializing reads with writer-lock creation would introduce filesystem effects
into reads and unnecessary contention. Allowing delete sharing on every private
writable handle would weaken the separate durable-owner pin contract.

## Consequences

Concurrent reads can return the previous credential, consistent with already
issued secret values. Deterministic native tests retain a reader across set and
unset and verify both old and new bytes. Files tests also prove a private writable
handle still refuses replacement.
