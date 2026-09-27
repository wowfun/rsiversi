---
name: Local Web launch with device identity and one-use browser handoff
---

## Problem

Requiring a custom Profile, listener flags, a second terminal and a copied device
receipt makes local Web startup harder than launching the terminal application.

## Decision

The product supplies an ordinary Web Profile with loopback defaults and bundled
assets. A local operator launch rotates one managed device credential without
changing its principal and grants configuration authority through the existing
grant owner. An existing principal receives its grant before conditional token
rotation, preserving its old working credential if authorization fails. The
registry checks the observed principal under its commit lock; deletion and
recreation cannot swap in an ungranted device. A first creation has no existing
credential and retires its exact issued token on failed authorization. A ten-minute one-use fragment ticket transfers that authority to
the browser cookie. The HTTP adapter consumes a narrow injected exchange
capability and never decides product identity or grants.

## Alternatives considered

A reusable process token in query parameters simplifies reopening but exposes a
longer-lived credential to request URLs. Unauthenticated loopback access loses
device attribution. Requiring manual registration retains unnecessary startup
steps. Creating a device on every launch strands browser drafts.

## Consequences

Local Web startup is an explicit grant operation; later revocation remains
authoritative for that run. Rotation invalidates old cookies while preserving
the draft principal. Browser reopening can use a valid cookie; an expired unused
launch link requires a fresh explicit launch. Remote deployment remains an
explicit Serve composition. Contract tests cover ticket consumption, revocation,
stable identities, socket-derived origins and browser handoff.
