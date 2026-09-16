---
name: Session-bound resource discovery and explicit skill reads
---

## Problem

The workspace skill catalog asks the model to load selected instructions, but
direct Human invocation alone does not provide model reads or application
previews through the selected Agent composition.

## Decision

Bounded, cancellable resource list/read callbacks live in the immutable
contribution catalog. Each read retains the actual draft or resident composition
pin and receives its authoritative Header. Registrations remain immutable;
request state belongs to the read owner. Projection callbacks remain pure.

Workspace Context owns discovery, trust, precedence, metadata validation and
body reads. Human discovery follows user-invocable; the ordinary skill_read Tool
follows model-invocable and obtains its Header from AgentCallerAuthority. Both
use the existing process-wide blocking-job admission. Only direct Human input
can produce UserSkillInvocation Facts. This extends the body-read rule in the
[workspace trust decision](../architecture/2026-09-03-workspace-context-trust.md)
without changing workspace trust or allowing model text to impersonate a user.

## Alternatives considered

A current-Host skill API would lose the Session's selected implementation.
Adding one feature-specific field per resource family to AgentCompositionPin
would duplicate lifecycle plumbing. Performing I/O in Projection would violate
its pure captured-state contract.

## Consequences

Human and model invocation flags are independent. Untrusted projects contribute
no skills. Reads do not change old pins, write Facts, or execute commands. Closed,
cancelled, malformed and capacity-limited reads have explicit outcomes. Actual
blocking jobs retain their admission until completion after waiter cancellation.
TUI and GUI preserve draft text, focus and exact request identity across reads.

### Trade-offs

A pin freezes implementation ownership, not filesystem bytes. Every read must
revalidate body identity against selected metadata. Cold recovery follows the
existing composition preparation contract; a process-local pin is not a durable
artifact locator.
