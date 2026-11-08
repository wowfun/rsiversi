---
name: Separate standing sandbox access from per-tool approval
---

## Problem

Selecting `danger-full-access` forced approval in both Turn submission paths.
Settings, Fact and Program validators independently rejected the same mode with
approval disabled. The standard preset also required approval. Thus a caller's
explicit standing access choice prompted again for each otherwise allowed Tool.
The executor regression reproduced two reviews for two calls despite a false
Session approval flag.

DSH's sandbox escalation service returns the current mode directly when a Tool
repeats it. A temporary widening instead requires a one-shot approval. Its Bash
Tool uses that service rather than deriving approval from the current mode.

## Decision

Resolve sandbox mode and the explicit approval flag independently, as specified
by the [Agent security contract](../../../../crates/rsi-agent/docs/security.md).
The standard unconfined preset selects standing access without per-Tool approval;
the default remains workspace-write. Preserve explicit approval requirements,
policy ask/deny decisions and Tool-owned escalation checks. This follows the
same policy ownership as the [application Tool decision](2026-11-04-application-tool-policy.md).

## Alternatives considered

Keeping a special unconfined exception in each validator duplicates policy and
blocks the same explicit choice at several boundaries. Removing all Tool
approval would also remove intentional user and contributor decisions. A second
cached consent mechanism would add state when the existing mode and flag already
express the choice.

## Consequences

New Turns and inherited workflow permissions retain their selected pair through
durable replay. Existing Sessions and accepted Turns retain their recorded flag;
no history rewrite or new persisted representation is needed. Explicit approval
and restricted sandbox enforcement remain separate contracts. No approval cache,
extra copy of settings or new IPC round trip is introduced.
