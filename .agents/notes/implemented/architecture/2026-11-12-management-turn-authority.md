---
name: Management Tools use actual Turn authority
---

## Problem

Persisted Agent leaf grants duplicate native Tool approval and do not represent
the calling Turn's current authority. Separately running application code has an
unconfined execution environment, which cannot establish permission for a
restricted Turn to install or change Host code.

## Decision

Management Tools declare a trusted full-access requirement. The executor compares
it to the actual Turn policy: Full access admits it directly; other modes need
per-call approval. Independent native approval requirements remain authoritative.
Application and Portable Tool protocols carry this declaration outside model-visible
schemas. DSH management delegates approval to this boundary without another
creator grant or application approval prompt.

This supersedes the Agent-grant clauses in
[Host leaf management](2026-09-20-host-profile-leaf-management.md) and extends
[application Tool policy](../simplification/2026-11-04-application-tool-policy.md).
Human clients still require Local-issued scoped grants. Both paths share reviewed
source preparation and publication. The retained-preview and receipt model is
superseded by [current-plugin recovery](2026-11-14-current-plugin-recovery.md):
preview is stateless and commit revalidates the change and review digest.
Saving Host source does not itself replace an Agent generation.
The [product owner](../../../../crates/rsi/core/README.md) owns the current contract.

## Alternatives considered

Keeping both Agent grants and native Tool approval repeats authorization without
adding confinement. Trusting an application's execution environment would bypass
restricted Turn policy. A name allowlist cannot describe third-party management
Tools and is not a substitute for trusted registration.

## Consequences

Agent grants are no longer valid persistent configuration. This pre-release change
adds no migration path; old state must be explicitly reset or corrected. Human
grant semantics remain unchanged, but the storage domain version marks the removal
of Agent grants rather than interpreting the older schema under new authority.
Local, Device and Agent in-flight work have separate bounded allowances, reserving
operator recovery capacity without a per-Session quota map. Publication remains
serialized; completed results retain no slots or receipts. Native Tool work remains
owned through completion. After a lost reply, callers refresh the catalog and
review current state; this cannot prove the original operation's outcome. Tests
exercise restricted and Full-access Turns, independent approval, class isolation,
digest revalidation and catalog refresh after reply loss.

