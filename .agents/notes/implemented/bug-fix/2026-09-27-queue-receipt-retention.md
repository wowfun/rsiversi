---
name: Queue edits retain outcomes without a lifetime admission quota
---

## Problem

A fixed lifetime receipt allowance permanently disables queue editing in a
long-lived Session, even when the mailbox itself remains small. Rejections also
need stable retry semantics after unknown outcomes and restart.

## Decision

The [Store contract](../../../../crates/rsi-agent/store-protocol/README.md) retains
successful and rejected operation receipts until Session deletion without a
lifetime count limit. Each receipt, append batch, payload read and concurrent
admission remains bounded. Indexed receipt and message-identity probes avoid
loading prior history or message bodies merely to check existence.

## Alternatives considered

Dropping rejected receipts permits a retry to produce a different result. Eviction
without an expired-operation protocol makes an old identity executable again.
Counting only successful edits postpones the same permanent limit. None is a
safe substitute for exactly-once outcome reconciliation.

## Consequences

Disk history grows with admitted operations, like canonical controls; receipt
lookup does not require resident history or hot-path count scans. A known domain
rejection settles its receipt. A conflicting reused operation identity retires
the GUI's queue intent without clearing the composer or fabricating a receipt.
Cold Store validation remains proportional to canonical history and repeats after
proof-cache eviction. It compares receipt-index coordinates to the current
canonical record instead of decoding that body twice. Expiring a negative cache
would not remove the need for durable rejection receipts or an explicit expired-ID
protocol; it is not a substitute for this integrity check.
