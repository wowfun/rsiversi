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

Shared block keys use canonical structured identities: direct Turn input and
claimed Message input have different kinds; model blocks include Turn, effect
and content index; Tool blocks include Turn, effect and the complete retained
result identity. Renderers consume keys opaquely and may split a semantic block
into presentation items. They cannot pair different Tool registrations or calls
merely because an effect string matches.

Tool metadata retains only bounded identity, name, exact sources, phase and
validated completed-output references. Intent is prepared; only a started Fact
means running. A rejection has arguments and rejection provenance without an
invented intent. A suffix-only result exposes its missing intent. Older intent
backfill repairs name and arguments without regressing the newest phase, result
or output references. Output identities pass the Process read validator before
being copied; arbitrary JSON strings are never retained as output authority.
Each renderer retains this metadata with its block, charges its owned capacity
to that renderer's budget, and drops it when the block is evicted.

Each block's `SourceIndex` retains at most 4,096 exact source identities, ordered
by Fact sequence and semantic content position. Text/image fields of one ordered
content array interleave by their original index. An exact duplicate returns its
existing position, including at capacity; a missing interior source is a distinct
insertion. Zero sequence and excess count are rejected without changing the index.
The index retains neither text nor Facts. Renderers remove corresponding source
entries whenever they evict a text span or Piece, choose the eviction direction,
and charge the index's allocated capacity to their own metadata budget. A highest
sequence watermark cannot substitute for this retained-source membership.

`MediaSource` borrows only a validated immutable image reference and its exact
closed source. Selection verifies the Fact sequence and media content kind;
input, Tool and generated images use distinct fields. The label contains only
canonical MIME, dimensions and byte length. It grants no byte-read authority.
Renderers retain bounded metadata text plus the source, then read the reference
again for a detail; they never retain image bytes or a Fact lease in the transcript.
Generated images have their own Turn/effect/output-index block identity.

Tests cover exact-source mismatch, closed wire fields and lossless sequences,
UTF-8 boundaries, large nested JSON windows and early serializer termination,
and independent Tool/process failure classification. Application tests cover
Session binding, cancellation and renderer-specific retention separately.
