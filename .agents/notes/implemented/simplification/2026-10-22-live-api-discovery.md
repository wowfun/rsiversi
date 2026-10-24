---
name: Live API discovery and fixed Portable grants
---

## Problem

Frozen catalogs and proxy-construction gates prevent existing clients from using
restored operations. Discovering authority from the current directory can silently
expand a narrowed capability.

## Decision

Typed clients are always constructible proxies. Feature activation and refresh
observe the current catalog and test the keys they consume. Connections require only foundation identity.
Session targets and Portable exports hold explicit immutable complete-key grants;
Portable forwarding resolves current single-key policy per call. Local composition
requirements continue to validate local wiring.

## Alternatives considered

A capability watch, revision negotiation and cached policy add lifecycle state.
Requiring every business domain at connection time prevents optional adapters from
being removed. Returning frozen policy from export creation recreates the original gate.

## Consequences

Discovery is advisory and can race registration. Final dispatch owns admission,
with distinct unknown, retiring, protocol and shape failures. This adds no generic
mutation replay; domain-owned receipt reconciliation retains its own semantics.
Portable pays one small policy query per business call; ordinary typed calls do not.
