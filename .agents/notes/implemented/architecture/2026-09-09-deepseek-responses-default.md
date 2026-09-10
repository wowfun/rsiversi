---
name: DeepSeek defaults to stateless Responses through shared bounded translation
---

## Problem

The product requires OpenAI Responses as its default Language protocol, including
DeepSeek. Reusing OpenAI's stored-response assumptions would advertise remote
state that DeepSeek does not implement. Maintaining a separate copied Responses
serializer and parser would duplicate the most sensitive admission and stream
validation code.

## Decision

DeepSeek defaults to Responses with explicit Chat selection. Its ordinary
provider factory freezes the chosen protocol in the prepared model snapshot;
it never falls back between protocols. The shared Responses adapter accepts
typed endpoint options for path, instruction role and stored/stateless state.
These options leave Images URLs and the default OpenAI behavior unchanged.

Stateless Responses carries plain reasoning in complete request history, emits
no response-id replay extension, and rejects stored-response extensions and
deferred operations before dispatch. DeepSeek owns its stricter media and
custom-Tool admission. The [provider contract](../../../../crates/rsi-ai/deepseek/README.md)
owns current capabilities; [shared Responses](../../../../crates/rsi-ai/openai/README.md)
owns bounded translation and stream validation.

## Alternatives considered

Using a previous response identity on a stateless endpoint would silently lose
the promised remote state. A copied provider parser would diverge in failure,
media and output limits. Protocol fallback could repeat an effect and obscure
which protocol a prepared request used. Reclassifying durable Agent inputs would
move provider policy into a product that must remain provider-neutral.

## Consequences

The default changes future DeepSeek preparations, while explicit Chat keeps its
wire behavior. Developer instruction mapping follows the existing
[provider-role decision](../bug-fix/2026-09-09-chat-developer-role.md): DeepSeek
cannot promise independent system/developer precedence. Plain reasoning is
distinct from visible output and bounded by existing Language stream limits.
The deferred parser's durable format remains unchanged because stateless
adapters reject deferred preparation and restore. Local HTTP tests establish
the exact default path, state behavior and event handling; opt-in live evidence
is tied to its separately frozen build.
