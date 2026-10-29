---
name: Workspace metadata and provider editors
---

## Problem

Workspace navigation lacked persistent names and an inspectable registration.
Model setup split credentials, provider declarations and default selection across
separate controls, so ordinary onboarding required knowledge of internal IDs.

## Decision

Workspace owns display names and recorded registration time alongside the stable
coordinate identity. Optional metadata represents an unnamed registration or an
unknown historical time; loading never fabricates a creation date. Rename keeps
coordinates, insertion order and creation time. Deletion retains its existing
registration-only semantics. The Web projection uses the same name in its grouped
navigation and composer picker. The [Workspace contract](../../../../crates/rsi-workspace/protocol/README.md)
owns validation and persistence semantics.

Provider setup follows the DSH card and inline-editor interaction while retaining
RSI's actual provider families and model limits. Copying unsupported Anthropic
options, per-model image switches or display-name fields would promise behavior
the inference adapters do not provide. A provider card's status means applied
configuration, not a successful remote model request.

Explicit model discovery can use an unsaved key. Requiring a credential write to
inspect a provider's model list would make a read unexpectedly durable. The
[configuration API](../../../../crates/rsi/configuration-api/README.md) instead
carries the transient key in a separate request envelope, excludes it from the
echoed identity, and keeps the existing grant, cancellation and network bounds.
Credential, provider and default-selection writes remain independent outcomes;
the editor retains the opening provider revision and preserves fields it does
not edit. Discovery candidates require an explicit Add before they become part
of the desired model catalog.

## Alternatives considered

A browser-local name would disappear on another client and contradict the
Workspace registry, so metadata remains with that owner. Inferring creation
time from directory metadata would describe a different event. Saving a key
as a prerequisite for discovery would change durable state before Apply.

## Consequences

Workspace and discovery wire contracts advance together with their consumers.
Older records retain unknown creation time. Provider status remains separate
from network health, and a key can be saved even when a subsequent provider
write fails; the editor reports that outcome and retries only unfinished work.
The first provider can supply a default model without a separate setup step.
