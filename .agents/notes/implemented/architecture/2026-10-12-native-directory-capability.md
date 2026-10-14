---
name: Native directory capability boundary
---

## Problem

The no-follow acquisition library returned cap-std directories and iterators.
Consumers could then accidentally use cap-std's full-path operations, whose
Windows implementation cannot preserve the retained-handle guarantee. A
Windows implementation behind those public types would retain that authority
leak even if its initial root acquisition used native relative handles.

## Decision

The [native Files library](../../../../crates/rsi-files/native-fs/README.md) owns
opaque directory, iterator and entry types. Public enumeration exposes a native
name and closed kind without an entry path or public dereference to cap-std.
Unix directories retain a native file so an already opened object can become a
directory capability without resolving its name again. cap-std remains private
enumeration machinery. Explicit Unix handle transfer and borrowing remain
available to consumers that own their handle-based policy. The library contract
owns platform acquisition, enumeration and creation mechanics.
CAS owners also use retained parents for private staging, non-replacing hard-link
publication, cleanup and synchronization. Native namespace pin acquisition does
filesystem work outside the process registry mutex and reconciles concurrent pins
before registration. Content identity and crash ordering remain Store policy.
This closes only the library's exported capability surface. The non-Unix
workspace-context implementation still constructs its own ambient cap-std
directories and uses path operations; it is outside this boundary and does not
establish Windows retained-handle confinement. Its owning source contract and
future native admission must address that separate fallback.

## Alternatives considered

A public alias or Deref would preserve the unwanted path API. Reimplementing
Unix enumeration would duplicate working handle mechanics without addressing
the boundary defect. Materializing all names inside the library would take
bounded discovery and cancellation policy away from its consumers.

## Consequences

Affected Unix consumers use the library-owned capability and lazy iterator.
Enumeration retains its original authority after directory rename/replacement
and after the caller drops the directory. Windows enumeration consumes checked
multi-entry records from one bounded native buffer; it exposes a lazy entry at a
time without materializing a directory inventory or owning caller cancellation.
Windows read-only library mechanics
do not establish the ordinary Files provider's namespace, version or paging
contract. Private-data owners must still validate existing objects. Delete sharing does
not remove NTFS's ancestor-rename restriction with retained descendants.
Consumers' permission, identity, capacity and lifetime checks retain their own
authority. Durable and wire path encoding remain outside these native mechanics.
