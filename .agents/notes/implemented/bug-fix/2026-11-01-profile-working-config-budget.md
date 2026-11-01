---
name: Bound live Profile configuration before final patch settlement
---

## Problem

A final-size check allowed many large live leaves and staged replacement values
to accumulate before later patches reduced the finished Profile. JSON measurement
also allocated a complete serialization solely to count bytes.

## Decision

The [Profile contract](../../../../crates/rsi-meta/profile/README.md) admits live
and staged configuration against twice the final configuration budget, releases
replaced charges, and counts bounded serialization without a second full buffer.
The same ledger covers file and public linked inputs.

## Alternatives considered

A final-only bound preserves every shrink-later program but does not bound live
retention. Charging only the final budget rejects useful temporary replacement
states. An unlimited staged-patch allowance recreates the original amplification.

## Consequences

A program exceeding the working bound fails even if a future patch would shrink
it. One expression is materialized before admission and remains subject to the
existing per-value and evaluator bounds. This is an explicit pre-release resource
contract, not a claim that the final byte budget equals total process memory.
