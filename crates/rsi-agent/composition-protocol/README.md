# rsi-agent-composition-protocol

This package owns the process-local interface between Agent composition,
session drafts, Kernel, and Executor. An `AgentCompositionPin` carries one
validated preset identity, one exact Profile source digest, one immutable Tool
Runtime, and opaque lifetime ownership for the standing generation. Consumers
cannot obtain a registrar, Profile resolver, Scope, or provider catalog from a
pin.

`AgentComposition` reports the effective default identity and resolves the
current healthy generation for one durable preset identity. Both operations
come from the same standing catalog authority; callers receive neither that
catalog nor its Settings adapter. `AgentSessionDraft` is move-only and
process-local: switching first acquires a complete replacement pin, then
atomically replaces the draft header and pin. Consuming the draft produces
`PreparedFreshSession`; dropping a draft has no persistence semantics. Kernel
owns the transfer of this value into resident session state and resolves cold
resumes through `AgentComposition`.

This package contains no preset filesystem discovery, generation construction,
Store access, Profile catalog, executor loop, or CLI behavior.
