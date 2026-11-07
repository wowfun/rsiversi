---
name: Local Profile failures retain plugin causes
---

## Problem

A Windows SQLite activation failure reported only that a Profile instance
failed. The actual private-directory ACL error was present in Meta's Fiber
state but discarded by Profile. Diagnosing the failure required a separate
direct Runtime executable. Cordis retains the caught activation reason and
logs it at the Fiber boundary (`vendor/cordis/src/fiber.ts` in the local DSH
reference); its loader (`vendor/loader/src/config/entry.ts`) also reports the
original error.

## Decision

Profile preserves plugin preparation and activation diagnostics for the local
operator, alongside the responsible instance. Existing diagnostic byte limits
still apply. Profile does not serialize configurations or expression source;
trusted plugin authors must redact secrets in the errors they return. The
[Profile contract](../../../../crates/rsi-meta/profile/README.md) owns this rule.
Remote Profile presentation continues to map closed failure categories and
does not forward local diagnostic strings. Runtime inspection and observed
child lifecycle categories remain redacted.
This refines the retained status described by the
[Profile input replacement decision](../architecture/2026-09-10-profile-input-replacement.md);
it does not change its executable-resource release ordering.

## Alternatives considered

Logging a second copy while returning a generic error would split the causal
information between two channels and require a configured logger to diagnose
bootstrap failure. Discarding all plugin text prevents ordinary operators from
fixing actionable local failures. Removing directory protection would change
the filesystem authority contract without addressing the diagnostic defect.

## Consequences

Startup, reload, rollback and static generation errors retain actionable causes
without another log store or process round trip. A plugin that embeds a secret
in its own error can expose it to the local operator; Profile cannot infer which
substrings are secrets. Public remote error categories remain unchanged.
