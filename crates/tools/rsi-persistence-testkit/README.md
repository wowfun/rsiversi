# rsi-persistence-testkit

This test-only library normalizes serialization schemas for owner-local durable
format gates. It uses the actual `JsonSchema` serialization view, retains object
property order, removes schema annotations, and replaces Rust definition names
with encounter-order identities. Fields named `title` or `description` remain
ordinary fields. It owns no runtime or product format. Its payload fixture helpers
capture the supplied type's serde encoding with a structural schema and checksum,
refusing to replace an existing fixture. This proves codec stability, not execution
of a production storage writer or its envelope. Those need owner integration tests.
Verification checks the owner's current version, schema, exact decode/re-encode
bytes and schema conformance of every sample.

Each persistence owner declares its version, codecs and immutable writer corpus.
Schema equality cannot establish semantic or prefix-digest compatibility: owner
tests must also replay or explicitly refuse old samples and check exact bytes.
The payload gate enumerates every version of its named codec. Old-version fixtures
remain immutable and are protected by `verify-persistence` retention checks. When
an older version exists, the macro requires an owner callback that exercises its
reader or refusal on that fixture; absence of this coverage fails the gate.

For a new payload format, keep the previous fixture and select a new versioned
path. From the repository root, run
`cargo test -p OWNER --lib persistence_PAYLOAD::capture_current_payload -- --ignored`
with the actual Cargo package and payload module names. Narrow the filter to the
new payload; capture refuses existing files. Samples are synthetic typed states
encoded by serde, not database files or evidence of effect execution. Owner
lifecycle and storage integration tests cover those separate obligations.
Markdown procedures and provenance remain editable; the retention gate protects
the non-Markdown byte evidence.
