# rsi-agent-session-protocol

This package owns the exact pre-release durable Session format: immutable
headers, bounded identities, append-only Facts, and one terminal outcome per
turn. It is a data contract, not a Runtime service or transport.

Language, Image, and Tool effects follow explicit intent/start ordering. A
direct Image request is durable before preparation, each successfully imported
image is committed as an ordered `MediaRef` Fact, and a later failure terminates
as `partial_failed` with those already-durable refs. Facts never contain media
bytes, resolved credentials, filesystem locators, or live capabilities.
An unconfined (`danger-full-access`) frozen profile is valid only when live
approval is required; this cross-field invariant is enforced on construction
and deserialization.

Custom deserialization revalidates nested protocol values, exact format,
identifiers, paths, diagnostics, Fact size, and sequence rules. Older formats
are rejected; this pre-release contract has no migration or compatibility
reader. A constructed immutable `SessionFact` retains its exact compact-JSON
length as an in-process validation proof; batching and Store admission trust
that proof instead of serializing the same typed value again.
