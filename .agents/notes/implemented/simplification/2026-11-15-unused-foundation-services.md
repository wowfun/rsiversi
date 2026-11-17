---
name: Remove unused permission and projection services
---

## Problem

Standard composition activated a frozen permission-preset map that no maintained
consumer resolved. Actual permission policy comes from Session settings and is
enforced by Approval and Sandbox. The generic Projection registry likewise had
no maintained registrants or readers outside its own tests; Session and GUI
projections use their owning typed interfaces instead.

## Decision

Remove both families, their catalog factories and Local markers, the unused
permission-map activation, dependency entries and CI package selectors. Keep
permission enforcement and Session/UI projections with their existing owners.
This supersedes only the permission-preset service portion of the
[foundation composition decision](../architecture/2026-08-27-foundation-first-plugin-composition.md).

## Alternatives considered

Keeping the services as speculative addon extension points preserves two
unconsumed contracts, their validation, registries and tests, and an unnecessary
Host activation. No maintained composition exercises those interfaces. Moving
them into another generic registry would retain the same obligation.

An absence of repository callers does not prove that external consumers cannot
exist: unpublished crates can still be path or Git dependencies. This is an
intentional pre-release API removal, justified by the maintained source graph,
not a compatibility claim based on `publish = false`.

## Consequences

Standard Hosts no longer allocate or activate the unused permission map. Generic
projection leases and bounds disappear; actual typed projection behavior and
permission checks remain unchanged. Custom source addons or Profiles referring
to these removed APIs/factories must use the existing owning contracts instead;
there is no compatibility alias. Composition behavior, metadata, lockfile and
documentation checks cover the remaining graph.
