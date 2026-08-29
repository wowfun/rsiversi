# rsi-tools-protocol

This package owns bounded process-local tool definitions, schemas, calls,
results, and executor/runtime contracts. Canonical JSON values are distinct
from ordered model-facing text and Media references. Every typed JSON value is
bounded by encoded bytes, nesting depth, and node count; callers importing a
Tool definition into a narrower capability must revalidate that capability's
own limits. Model-produced numbers are accepted only when their exact decimal
value survives the canonical `serde_json::Value` representation. Serializable
calls and execution policies revalidate those invariants during
deserialization rather than relying on a later runtime consumer.

It contains no registry implementation, policy, approval, durable logging,
sandbox, provider wire, or plugin lifecycle.

The caller supplies one bounded invocation identity when preparing a call.
Durable orchestrators use their own effect identity; the Tool layer does not
guess session or turn structure from a model-produced call ID.

A Runtime generation admits at most 64 active Tool registrations. Each
registration timeout is within 1..=600,000 milliseconds. The current
pre-release result shape is exactly `{ value, content, is_error }`; image
content contains a durable `MediaRef`, not an inline blob or status envelope.
Model-facing text rejects C0 terminal controls other than tab and line breaks.
The removed v0 status/blob tool-envelope shape is not forward-compatible and
has no migration reader.
