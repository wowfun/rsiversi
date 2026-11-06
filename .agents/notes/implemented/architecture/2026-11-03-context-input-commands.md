---
name: Context input and supplemental seed ownership
---

## Problem

DSH Side Chat injects a conversation boundary before separately queuing a human
question. The native command publication path already owns non-waking inputs,
atomic receipts and draft staging, but requires a domain replacement even when
the operation has no domain state.

## Decision

Commands may publish inputs without replacements. The complete nonempty input
batch is bound to the existing commit digest and request receipt. First draft
publication works with an empty domain catalog. The ordinary context-input
plugin exposes this behavior through the existing command service and uses
native command provenance. Kernel remains the scheduling and publication owner.

Command inputs separately retain bounded original message attribution and ordered
typed text, image and file blocks. DSH's Agent inbox stores the complete supplied message; flattening
its text or replacing its ID/source changes removal and plugin classification.
Attribution belongs in the accepted native input and consumed Fact so cold replay
and independent forks do not depend on a live JavaScript owner's lookup table.
The native invocation digest and ordinal still determine execution identity.
The context-input command verifies file and image objects at its external
argument boundary using the selected Media capabilities. It then publishes the
same typed content used by ordinary messages. Non-waking scheduling is independent
of content type; a text-only command transport would introduce a second input
contract without an execution or durability reason.

Direct application/user submission retains the same bounded attribution. DSH
`inject()`, `followup()` and `steer()` use one inbox mutation contract; delivery
and waking are independent of body size or authorship. Native direct ingress
therefore accepts `NextStep`, `NextTurn` and `Steer`, using Kernel's existing
mailbox routing. The DSH bridge uses this ingress for all new input rather than
wrapping injection in a command. This removes command discovery, a second
invocation encoding and the command-specific content ceilings. The context-input
command remains available for callers requiring atomic command publication.
`Human` names interactive ingress, not proof of human authorship.
Presentation identity must not double as the native idempotency key: the same
original message ID can legitimately be reused after an earlier acceptance is
consumed. Queue controls and consumed Facts retain both identities.

An accepted direct non-waking input publishes the Session and its mailbox entry,
without starting a Turn or resuming a paused inbox. DSH's `ReactLoopInbox.mutate`
likewise appends accepted context to the Session regardless of waking. Keeping
such content unpublished would require an additional draft receipt and recovery
owner; durable native acceptance instead uses existing retry, withdrawal and
restart semantics. Empty drafts remain unpublished; first acceptance ends their
draft-only preset replacement along with fixing the Header. Clearing accepted context
does not retract the durable Session. New-Turn options and model availability
are irrelevant to held input; execution validates the eventual waking request.

## Alternatives considered

A dummy counter or last-request domain would duplicate receipts solely to satisfy
an incidental restriction. A second application inbox would split scheduling and
recovery ownership. Steering the boundary would wake idle Sessions and merge its
intent with the question, unlike the original plugin's separate operations.

Open-turn Side Chat also requires a distinct supplemental seed: Better Sidebar
clears the fresh child's inbox after creation, while DSH's inherited log survives
that operation. Treating this seed as an injected message would lose it. A
separate native context-seed domain retains real pending state until its atomic
entry as a context contribution. This uses existing domain publication and
recovery rather than a JavaScript-only buffer or a synthetic parent terminal.
Command arguments admit 64 KiB of encoded JSON: the upstream 8,000-character
snapshot can exceed the former 16 KiB limit with Chinese text or JSON escaping.
The API envelope has room for the full argument budget plus invocation metadata.

## Consequences

No new Header field or compatibility queue is needed. Command and
baseline commits can have zero domain updates; all other provenance and size
rules still apply. This primitive does not by itself supply the original DSH
synchronous Agent facade or its HTTP response publication barrier.
Supplemental seed state follows the native draft lifetime before publication;
it is not a promise to recover unpublished drafts after Host retirement.
The durable Session format advances for attribution on both input paths; no
old-format migration is promised. Native attribution supplies the durable
foundation; the application journal separately owns synchronous inbox acceptance
and its publication driver.
