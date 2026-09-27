---
name: Host directory picker authority and bounded native work
---

## Problem

Workspace registration accepts known paths but does not authorize browsing a Host
home or creating directories. Browser paths cannot represent non-UTF-8 names, and
a timed-out blocking filesystem call does not mean its actual work has ended.

## Decision

The standard product owns the picker and its shared API client. A Host
configuration grant authorizes browsing and single-level creation. It resolves aliases to
physical paths and operates through retained no-follow handles. It retains two actual
work slots and leases until syscalls finish, even after the five-second caller
deadline. Non-Unix reports Unsupported and retains manual registration.

## Alternatives considered

Putting the picker in Workspace would conflate registration with filesystem
access. Reusing recursive mkdir would mutate undisplayed parents. Releasing a
slot on timeout would conceal unbounded blocked work. Pretending non-UTF-8 names
are valid strings would register a different path.

## Consequences

Real temporary-directory tests cover aliases, single mkdir, replacement, hidden
and unrepresentable names, bounds and cancellation. Native and Worker clients use
the same API; ungranted calls fail before filesystem work. Unknown creation is
explicit and has no automatic retry.


A stalled kernel filesystem call can delay shutdown and grant revocation. This is
an honest native limitation rather than permission to release live authority.
Listing is a bounded sorted window, not a snapshot or complete paginated catalog.

Desktop explicitly selects the same API client plugin after its native connection;
Worker selects it over HTTP. Presence in a child transport catalog alone does not
publish the capability to the application. Linux window validation caught and
prevented this otherwise silent manual-path fallback.
