---
name: Canonical history pages and explicit execution proofs
---

## Problem

Cold read-only history performed the same whole-Session index replay as execution
recovery. A measured 35-Session Store capture spent 211 ms establishing proofs
before returning canonical pages; its warm capture did no such replay. Derived
mailbox, domain and turn indexes neither select nor interpret those pages. Kernel
already prepares each Session before admitting payload memory, so implicit proof
on every raw reader couples display availability to unused execution indexes.

## Decision

The [Store protocol](../../../../crates/rsi-agent/store-protocol/README.md) owns
the split between canonical interval validation and explicit execution proofs.
It replaces the raw-history part of the
[first-access decision](2026-09-01-agent-store-validation-and-read-snapshots.md).
Every canonical reader retains bounded decoding, coordinate agreement and snapshot
consistency. Indexed selection, mutation and explicit preparation retain full
relational validation. Kernel retains its preparation lease; ACP cold restore
also prepares before interpreting baseline controls as composition authority.

DSH's Session persistence handle reads the canonical log without consulting
derived execution indexes. Its JSONL backend validates stored events and memoizes
the decoded log; it does not provide this SQLite interval implementation. RSI
adopts the separation of canonical history from derived execution state while
keeping bounded page admission instead of retaining another complete log copy.

## Alternatives considered

A DSH-only fast-read API would duplicate Store semantics and leave native history
with the same cost. Enlarging the proof cache retains more state and still replays
every cold Session. Dropping record validation would trust durable untyped bytes.
Removing preparation from execution would let damaged indexes authorize work.
All four alternatives fail the intended ownership boundary.

## Consequences

Valid history can be displayed even when an unrelated derived index is damaged.
Canonical reads do not promise whole-Session integrity and do not warm proof
caches. They detect corruption only in their selected interval and required
watermarks; complete verification remains explicit. Execution and writes still
refuse the same damaged Session. A read-time suffix digest remains metadata,
not a freshly recomputed proof. No durable representation or additional payload
copy is introduced. Removing measured work is not itself evidence of end-to-end
latency or acceptance on a 512 MB device; those require separate measurements.
