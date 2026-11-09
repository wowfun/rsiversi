---
name: Preserve the failing Agent Profile instance in diagnostics
---

## Problem

A live DSH history-open check returned only “activating an Agent contribution
failed”. Profile had already retained the failing InstanceId, but Agent
composition discarded it, preventing the operator from locating the contribution.

## Decision

Preserve the validated Profile instance alongside the existing failure stage in
Agent composition errors. The current contract belongs to the
[composition README](../../../../crates/rsi-agent/composition/README.md).

## Alternatives considered

Forwarding the complete plugin error would also forward arbitrary diagnostic
text into remote Session errors. Adding another diagnostic registry or retaining
failed generations would duplicate information already present in ProfileError.
The existing error can carry the useful bounded identity directly.

## Consequences

Operators can locate the failing leaf without reconstructing the activation.
Raw plugin diagnostics remain outside this error, and no failed generation or
additional runtime state is retained. Exact error-message assertions intentionally
change to cover the new identity and continued exclusion of private seed text.
