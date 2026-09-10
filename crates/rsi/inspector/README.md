# rsi-inspector

The product Inspector registers read-only `inspector.runtime`, `inspector.profile`,
`inspector.factories` and `inspector.native` version 1 operations through the
ordinary API registrar. All require local operator access: remote credentials
neither discover nor invoke them. The owning plugin withdraws and drains the
registrations during retirement. Inspection never loads artifacts, prepares a
factory, builds an Agent generation or applies a Profile.

The source supplies actual Meta inspection and Profile control observations;
this adapter projects those values without configurations, source paths, raw
failure strings, effect labels or service values. Runtime pages use Meta's Fiber
cursor and per-collection limits. Profile and frozen factory pages use bounded
integer offsets, at most 128 rows per request. Profile rows are preorder entries
with parent identities; disabled nodes remain visible. Native inspection reports
selection identities and actual Loader resource counts, including retained
failures. A missing native manager is explicitly unavailable.

All revisions, Runtime identities, supply/effect tokens, composition positions,
and byte counts use canonical decimal strings. Counts and page offsets remain
bounded JSON integers. Each request permits 1 KiB and each response 4 MiB of
encoded JSON under the existing API Data reservations (leaving Control capacity
for lifecycle operations); an oversized response
returns capacity failure, without silent truncation. Unknown request fields,
invalid cursors and out-of-range limits are rejected before source invocation.

Observations across Runtime, individual owners, Profile and native state are not
one atomic graph. Profile offsets apply to the observed revision; callers must
restart pagination when that revision changes. Frozen factory pages describe
linked and explicitly resolved declarations. Dynamic native selection is reported
separately and active generations also appear in Runtime pages. Descriptive
factory metadata does not attest prepare success or confer authority.

The terminal application consumes finite JSON documents for display. It performs
no automatic action based on their contents. Verify public API registration,
authorization, pagination, redaction and withdrawal with isolated fixtures, then
exercise the built product over its real local operator transport.
