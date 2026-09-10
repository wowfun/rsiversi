---
name: Concrete Chat adapters own developer-role compatibility
---

## Problem

Enabling ordinary persisted context exposes a live DeepSeek rejection of the
Chat `developer` role. The shared serializer previously assumed every Chat
endpoint accepted it. Changing Agent history or its builder to accommodate one
provider would mix provider syntax with durable execution semantics.

## Decision

Typed Chat endpoint configuration chooses the wire representation of Developer
messages. The generic adapter defaults to `developer`; DeepSeek selects
`system`. Translation occurs during wire serialization, preserving message
positions, content and the prepared semantic request identity. The
[DeepSeek adapter contract](../../../../crates/rsi-ai/deepseek/README.md) owns this
provider behavior.

## Alternatives considered

Reclassifying Agent input would contaminate every provider and replay path.
Dropping context would lose actual instructions. Using an undocumented
provider-specific reminder role would rely on an uncontracted precedence model.
Rejecting Developer messages during Prepare would leave standard workspace/time
contributors unusable on this provider.

## Consequences

DeepSeek cannot express separate system/developer precedence through this Chat
interface. Mapping both instruction roles to system is an explicit provider
limitation, not a claim of identical instruction hierarchy. Local HTTP tests
assert the exact transmitted order, role and text. The live coding failure is
retained separately from the corrected build's subsequent evidence.
