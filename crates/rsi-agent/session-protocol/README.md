# rsi-agent-session-protocol

This package owns the exact pre-release durable Session format: immutable
headers, bounded identities, append-only Facts, and one terminal outcome per
turn. It is a data contract, not a Runtime service or transport.

Every header carries one required `AgentPresetId`. Its lowercase
`[a-z0-9][a-z0-9-]*` grammar is also safe as a preset-directory segment, and
construction plus deserialization enforce the portable 255-byte segment bound.
The durable
value records which preset a session selected; process-local composition
generation handles are deliberately outside this format.

Each immutable profile carries a `TurnBudget`. The first protocol generation
uses repository hard maxima of 30 elapsed minutes, 64 provider attempts, 256
Tool calls, 65,536 generated Facts, and 64 MiB of generated Fact bytes; a
profile may only tighten them. Budget exhaustion is itself a nonterminal Fact
followed by the sole `budget_exceeded` terminal outcome, so interrupted
observers and recovery can classify the stop from durable history.
Both records validate that their frozen limit is positive and no greater than
the hard maximum for the named dimension; foreign history cannot widen a
budget while claiming it was exhausted.
The budget is mandatory in the current durable header encoding; decoding never
widens an omitted budget to repository maxima.

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

The protocol also owns the canonical rolling SHA-256 chain over serialized
Facts. Context projection and Store append accounting share that algorithm but
derive it independently from their own Fact inputs, so an opaque checkpoint
cannot supply its own provenance proof.
