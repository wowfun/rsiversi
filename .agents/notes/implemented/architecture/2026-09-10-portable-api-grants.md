---
name: Explicit Portable API grants and early retirement observation
---

## Problem

Native presentation code cannot receive a safe-Rust API trait object. The API
operation metadata already defines admission, effect ownership and byte bounds,
but Meta messages have their own framing and exact capability holders. A paused
subscription can also retain an outbound call while Meta waits for admission to
drain before running the adapter's deferred cleanup.

## Decision

Transport an explicitly selected API operation catalog through ordinary Portable
capabilities. Keep domain policy in its owner and use raw contiguous fragments for
JSON and binary data. Reuse the shared API connection's separate receiving and
retention pools, owned mutations and supervised subscription handoff. Complete
finite replies and clean stream endings require the actual Meta terminal result.

Expose an observation-only Context retirement signal at admission closure.
Importers use it to retire paused connection drivers before effects run; exporters
cancel reads while retaining admitted mutation jobs through settlement. The signal
holds neither admission nor the Runtime and adds no cleanup phase or callback.

Keep capability transfer scoped to the exact holder. A Local clone cannot retarget
authority. `export_api` lets an ordinary activating product plugin export an already
narrowed client in the same generation as its injected destination capability.
Native SDK channel host access retains the original callback lifetime while
allowing interleaved forwarding on the outer and nested channel orientations.

The Session domain supplies `SessionTargetClient`: it rejects creation, deployment
listings and another Session handle in the request envelope before dispatch.
Approval owners and cancellation subjects are validated by the Session domain
against that handle's Agent tree, which deliberately includes descendants. Presentation IDs never grant
Session authority. Header fingerprints and receipt validation remain in the
existing Session API handlers and clients.

## Alternatives considered

A second forwarding queue would duplicate byte and lifecycle ownership without
fixing the retirement ordering. Moving arbitrary effects before Meta's admission
drain would weaken all plugins' cleanup contract. JSON-encoding binary fragments
would inflate the wire bound and copy large results. A generic domain request
ledger would duplicate existing mutation receipts.

## Consequences

Portable subscriptions inherit the Meta service-call deadline and require explicit
domain reconnection. Hot transfer does not extend that deadline or authorize
mutation replay. The real native SDK fixture proves binary and domain-error
round trips, revocation without a native hard dependency, and Loader resource
drain on Linux. Malformed framing, unpolled importer retirement, and retained
64 MiB response slices have public-boundary tests. WASM compilation is separate
from browser execution, and these bridge tests do not establish a dynamic UI.
