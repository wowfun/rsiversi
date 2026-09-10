---
name: Expected native artifact identity before executable admission
---

## Problem

An installation record selects specific native bytes. Checking a returned
factory's digest after load is too late: the operating-system library loader and
ABI entry have already executed. Hashing the public source path before an
ordinary load also leaves a source-replacement window before private staging.

## Decision

NativeCatalog exposes `load_exact(path, expected_sha256)` alongside trusted-path
loading. The caller still selects the path and expected identity; the Loader
performs no manifest discovery or hash-to-path lookup. It validates the supplied
digest before admission or source I/O, then fences each digest chosen by the
existing source/rekey loop. That loop already requires the private staged copy
to match its selected digest before mapping. A mismatched source therefore never
reaches library mapping, ABI entry or durable cache publication.

Both entry points use the same catalog lease, load admission, per-digest gates,
private staging, callback budgets and finalization-failure fence. No alternate
catalog or load queue is introduced. Reusing a live module returns only the
expected immutable staged identity, independently of later public-path changes.

## Alternatives considered

A post-load comparison diagnoses a mismatch after unreviewed code ran. A separate
product staging implementation repeats the Loader's stable-copy and resource
ownership mechanics. Looking up arbitrary files by digest makes the Loader an
artifact manager. A second catalog bypasses retained-failure admission and quotas.

## Consequences

The expected digest identifies only top-level bytes, not transitive OS libraries,
authorization or a sandbox. Public tests cover malformed input before source I/O,
mismatch before callbacks/cache, exact and ordinary module reuse, source replacement
with an admitted waiter, and staging release. The existing internal retained-failure
fence test also exercises the exact entry point. Actual failed native finalization
and product installation remain separate acceptance surfaces.
