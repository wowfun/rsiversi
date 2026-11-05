---
name: Bind request Tool activation to the resolved model selection
---

## Problem

The executor captured application Tools before resolving the model for the
request. The process capture carried only the immutable Header, so DSH's
model-aware activation could not see the effective model or effort. Reading
the live application Agent instead would race a model-selection command and
could disagree with the native provider request or its retry.

DSH resolves its selection first and supplies that same value to Tools and
system-prompt assembly in `Agent.preStep`. This ordering is the required
foundation for application preparation hooks in the standard executor.

## Decision

Resolve selection from the claim's captured contribution horizon before
request Tool capture. Pass the typed selection through the existing source
contract and encode it once at the authenticated application boundary. The
selected catalog and model remain paired across provider retries; the next
series captures a new pair. No second Session read determines this choice.

## Alternatives considered

Header defaults ignore durable model commands. A current application Agent
read can observe a later command than the request's captured horizon. Resolving
the choice independently twice creates the same race inside native execution.
Passing only the model name loses provider and reasoning-effort activation.

## Consequences

The process capture contract is version 4 and requires the selection. It does
not gate DSH versions or hashes. Released registries without model-dependent
activation retain their ordinary scope semantics. This change does not itself
implement prompt-message rewrite/drop or the market pre-step callback; those
remain separate required preparation behavior.
