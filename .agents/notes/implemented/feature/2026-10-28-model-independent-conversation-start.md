---
name: Model-independent conversation start and DSH transcript navigation
---

## Problem

Opening a directory implicitly created a Session, so an unconfigured model
blocked workspace registration and disabled the composer. An empty resource
column and duplicated status controls also obscured the initial conversation.

## Decision

The standard Service owns first-use directory policy through the
[default workspace API](../../../../crates/rsi/workspace-default-api/README.md).
The generic Workspace registry keeps its existing authority and no product
Documents convention. Directory registration and blank conversation surfaces
do not create Sessions. A separate, revision-checked starting draft persists
text before a Session Header exists; sending transfers it to the existing bound
submission journal before clearing the captured starting revision.
Starting input has its own aggregate budget and paged recovery because endpoint
changes otherwise leave bounded individual records with unbounded total retention.
Incarnations prevent an editor that observed stored input from resurrecting a
removed record or overwriting its replacement. An editor that observed absence
has no stored input to resurrect; its later first edit is fresh intent and may
create a record only while that key remains absent. Tracking intervening
create/delete cycles for these blank editors would require per-key durable
history without protecting additional stored input. A global deletion epoch
instead conflicts unrelated blank editors, so neither mechanism is retained.
Explicit reset retires both
draft owners and all existing editors until reload; deleting only the Session
journal leaves pre-Session input behind.

Conversation and Activity tabs adapt RSI's existing turn presentation. Activity
shows the full existing execution record; it does not import DSH runtime state.
The conversation heading represents a successful terminal once; a typed terminal
identity avoids deriving this policy from English labels or opaque block keys.
Activity preserves chronological evidence. This partially supersedes the
[turn presentation decision](2026-09-26-turn-presentation-index.md), whose
partial-history, failure and candidate visibility rules remain.
This extends the earlier [React workbench decision](../architecture/2026-10-27-dsh-aligned-react-workbench.md)
with the requested DSH-style navigation while retaining that decision's rendering
and commit ownership. Current UI behavior lives in the
[presentation contract](../../../../apps/web/docs/presentation.md).

Session statistics reuse the existing metrics owner instead of summing a paged
transcript. Recorded attempt offsets support aggregate model time, TTFT and decode
throughput. Each footer reading opens its own anchored panel; missing context
subdivisions remain absent. The [conversation contract](../../../../crates/rsi/conversation/README.md)
owns timing semantics, and the versioned Session API owns their transport.

## Alternatives considered

A placeholder Session would require a fabricated model or weaker Session Header
invariants. Reusing the process CWD as a default workspace would make launcher
context decide user data placement. Keeping default policy in the generic
registry would make unrelated consumers inherit a product-specific convention.
A localStorage draft cannot provide atomic conflict checks across documents.
Combining both draft formats would couple pre-Session state to the larger frozen
request journal. Separate bounded stores retain their existing authority and
ownership; reset therefore reports either deletion failure instead of claiming
an atomic two-database operation. Legacy input above the new aggregate allowance
remains recoverable and may shrink until it fits.

## Consequences

Starting drafts have their own bounded IndexedDB format and endpoint/principal/
surface key. They preserve the established Session-bound journal and its receipt
rules. Attachments and captured references remain available after Session
creation because they require its real authority. Model configuration gates
sending, while typing remains independent. No unsupported DSH voice, feedback, branching or team controls are presented merely for visual similarity.

The initialization receipt survives removal of a registration, so deliberate
removal cannot silently recreate the default. Windows directory browsing uses
the shared native filesystem's fixed-local-NTFS and no-reparse admission; UI
alignment does not weaken that boundary. Browser disconnect retires document
credentials and transport, not an RSI account.
