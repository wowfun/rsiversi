---
name: Approval evidence and explicit pending-request reconciliation
---

## Problem

Opaque approval text hides the prepared action. A transport failure after answering
also leaves the client unable to tell whether the decision was received.

## Decision

Web and TUI show the prepared action and supplied execution evidence with bounded
argument and raw-request windows. Requested sandbox policy is presentation data,
not proof that an effect was sandboxed. The [Web contract](../../../../apps/web/docs/presentation.md)
owns dialog behavior. TUI reads pending state when opening each approval and keeps
its review until a known decision result. After an uncertain reply the decisions remain disabled;
Refresh request reads current pending state from the Session owner, without
replaying an answer. An unrelated unsupported question route cannot block a
successful approval read. The current attachment is rechecked after the read.

## Alternatives considered

Automatic answer retry can repeat an effect decision. Permanent disabling leaves
no recovery path. Renderer-cached pending state can reopen an already settled
request. Removing the raw request loses evidence useful for diagnosis.

## Consequences

The user can inspect and act on a freshly read pending request. Final admission
still arbitrates concurrent answers. A failed refresh does not assert completion;
it reports its actual read error. Settings replacement preserves keyboard focus,
and dialog close restores its original trigger.
