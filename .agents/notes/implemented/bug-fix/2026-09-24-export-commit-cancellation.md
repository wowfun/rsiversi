---
name: File export cancellation ends at persistence admission
---

## Problem

Dropping the future awaiting blocking persistence cannot stop its filesystem
operation. An outer cancellation select can report cancellation after replacement
has been admitted. A view lifetime also cannot own that operation's capacity.

## Decision

Cancellation belongs at the [native sink's](../../../../crates/rsi/session-export/README.md)
last reversible boundary. Existing application task trackers own the operation
because detached views cannot establish whether persistence ran. The
[Desktop](../../../../crates/rsi/desktop/README.md) also retains an open native
chooser: its callback API has no programmatic dismissal, and freeing admission
before that callback would permit orphaned or overlapping dialogs.
A server-issued reservation identifies cancellation before and after chooser
admission. Matching under the same owner lock prevents a late cancellation from
retiring a replacement operation. An independent control lane prevents ordinary
work from starving that cancellation. Destination-level serialization is not
introduced: atomic replacement keeps its documented last-writer semantics.

## Alternatives considered

Documenting the sink alone leaves client cancellation notices incorrect. Extending
Session wire errors would move a local filesystem policy into the service. A new
Job service would duplicate existing application ownership. Claiming a timeout
rolls back rename is not supported by the operating system.

## Consequences

Late cancellation can wait for filesystem completion and can still report a saved
file. The application cannot promise bounded shutdown while an admitted blocking
filesystem operation stalls. An open chooser can also delay application cleanup
until the user dismisses it. Abrupt process loss provides no success confirmation.
