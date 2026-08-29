# rsi-agent-store-protocol

This crate owns the mechanical durable seam for Agent sessions. A Store accepts
one immutable header, contiguous compare-and-append Fact batches, bounded
reads, session enumeration for recovery, and immutable CAS objects. Alongside
the canonical session sequence it transactionally maintains mechanical turn
membership and open/terminal indexes. Those indexes select durable bytes; they
do not apply effect transitions, classify recovery, or select a turn outcome.
See [the Agent architecture](../docs/architecture.md).

Session Fact reads, per-turn Fact reads, open-turn enumeration, and lexical
session enumeration are cursor-paginated with protocol-owned count and byte
limits. No Store method materializes an unbounded durable catalog.
