# rsi-agent-store-protocol

This crate owns the mechanical durable seam for Agent sessions. A Store accepts
one immutable header, contiguous compare-and-append Fact batches, bounded
reads, session enumeration for recovery, and immutable CAS objects. Alongside
the canonical session sequence it transactionally maintains mechanical turn
membership and open/terminal indexes. Those indexes select durable bytes; they
do not apply effect transitions, classify recovery, or select a turn outcome.
See [the Agent architecture](../docs/architecture.md).

Session Fact reads, per-turn Fact reads, open-turn enumeration, lexical session
enumeration, and lexical enumeration restricted to sessions with open turns are
cursor-paginated with protocol-owned count and byte limits. Startup recovery
uses the open-session index, so closed historical sessions do not impose Store
calls or decoding work. No Store method materializes an unbounded durable
catalog.

Context checkpoints are an optional cache, not canonical session state. The
Store preserves at most 64 MiB of opaque Context-owned bytes with a header
fingerprint, cursor, and lowercase SHA-256 digest of the folded Fact prefix, and
installs them only when that cursor exactly equals the durable tail and both
fingerprint and prefix digest equal Store-owned values derived from the
canonical header and Fact log. A missing, stale, corrupt, or unsupported
checkpoint never changes Fact replay semantics.
