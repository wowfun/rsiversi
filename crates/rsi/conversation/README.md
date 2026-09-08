# rsi-conversation

This pure native/wasm library owns conversation source semantics. It consumes
validated Agent Facts without retaining them, observation leases, a Runtime,
domain capabilities or renderer state. Terminal sanitization and source-to-cell
mapping belong to the TUI; blocks and DOM identity belong to Web.

`SourceRef` identifies an exact Fact sequence and a closed `FactField` within one
attachment. Serialized sequences are canonical positive decimal strings, so a
JavaScript bridge cannot round durable identity. Applications bind every source
read to the captured Session handle and application/detail generation. Sources
are neither a Session selector nor authorization to read another Session.

Field selection checks exact sequence and the matching Fact variant/content kind.
A Tool argument source cannot select result text or another JSON field merely
because the old numeric index matches. Missing fields are unavailable, including
a text field targeting an image. Model errors use their display text; structured
arguments, results, rejection and outcome details use pretty JSON.

Windows contain at most 256 KiB of raw UTF-8, with actual start/end byte offsets
and a continuation flag. A start inside a code point advances to the next boundary;
the end never splits a code point. JSON is serialized through a bounded skipping
writer and stops after enough bytes to establish continuation. It never creates
a complete serialized copy before truncating. Walking an omitted prefix remains
CPU work; this is a retained-byte bound, not constant-time random JSON access.
Renderers may impose smaller limits and must preserve the returned source offset
when sanitizing or mapping display text.

Tool outcome classification preserves Tool-owned errors separately from nonzero
process exits and signals. These values are presentation semantics over the
validated result, not a new execution policy or reconstructed process authority.

Tests cover exact-source mismatch, closed wire fields and lossless sequences,
UTF-8 boundaries, large nested JSON windows and early serializer termination,
and independent Tool/process failure classification. Application tests cover
Session binding, cancellation and renderer-specific retention separately.
