---
name: Host directory picker authority and bounded native work
---

## Problem

Workspace registration accepts known paths but does not authorize browsing a Host
home or creating directories. Browser paths cannot represent non-UTF-8 names, and
a timed-out blocking filesystem call does not mean its actual work has ended.

## Decision

The standard product owns the picker and its shared API client. An explicit execution location separates Local filesystem access from SSH targets.
A Host configuration grant authorizes Local browsing and single-level creation;
a current target Use grant authorizes the corresponding SSH operation. It resolves aliases to
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

The target path uses a reserved immutable helper entry and the existing native
handle algorithm. DSH `packages/host/directory-picker/src/index.ts` establishes
bounded single-level browse/create behavior, but does not establish RSI target
authority. A generic shell or unrestricted remote mkdir RPC would unnecessarily
widen this API. The selected directory may be a filesystem root, so applying a
workspace-only read scope would silently narrow the picker contract. The fixed
entry therefore runs under explicit broad picker authority, receives only closed
request data and target HOME, and cannot execute caller-supplied programs.
