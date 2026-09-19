---
name: Closed saved-codec diagnostics across plugin activation
---

## Problem

Composition deliberately replaces arbitrary plugin activation errors with a safe
category. Consequently a saved Domain whose codec its owner no longer accepts
fails closed but gives the user no way to distinguish it from an activation bug.

## Decision

The owner selects its exact saved codec through `AgentGenerationInputs::seed_state`.
A build-local channel retains the first mismatch as validated saved and expected
Domain identities. Composition rolls back and exposes that closed error with the
action to start a new conversation. A recorded mismatch also prevents sealing if
the plugin ignores its lookup failure. Cloned inputs share the diagnostic only
within that build; a subsequent build starts empty.

The [composition protocol](../../../../crates/rsi-agent/composition-protocol/README.md)
owns the API. MCP declares its required codec there; composition and Kernel stay
unaware of MCP schemas, versions, or migration policy.

## Alternatives considered

Forwarding activation error strings could disclose plugin configuration or secrets.
Matching an error string would couple unrelated owners to a diagnostic's spelling.
Interpreting MCP states in composition would reverse the ownership boundary.
Silently replacing a saved manifest would change the conversation's frozen tools.

## Consequences

Unsupported codec failures are actionable without exposing saved contents or
arbitrary plugin diagnostics. Missing or malformed state remains subject to the
existing error taxonomy. This is an exact-codec API, not a migration registry;
owners requiring migrations need an explicit contract for them. Protocol tests,
composition rollback/seal tests, and the standard Host codec-1 restore test cover
the diagnostic path, redaction, and isolation from healthy generations.
