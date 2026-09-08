---
name: Terminal Facts and control horizons commit as one boundary
---

## Problem

A Fact terminal alone cannot identify which Session controls belong to its
historical cut. Sampling the current control tail while creating a fork would
include commands issued after the selected Turn ended. Ordinary write-behind
and activation terminal paths also need the same correlation rule.

## Decision

The Kernel owns one terminal-marker producer for ordinary flushing, activation
completion and startup recovery. The [Store contract](../../../../crates/rsi-agent/store-protocol/README.md)
admits each terminal only with its exact final control marker in the same
Session append. Derived terminal indexes and immutable fork lineage retain
both canonical prefix positions. Store adapters validate correlation without
choosing Turn outcomes or running plugin callbacks.

Write-behind ends a batch at its first terminal and reserves the marker's
encoded bytes. Control writers hold Session submission admission and wait for
any queued terminal before sampling their control cursor. The flusher never
acquires that admission: a writer can already hold it while waiting for flush.
The existing nonterminal Fact-conflict retry does not rebase business control
sequences or change receipt identities.

Recovery commits one unfinished Turn at a time. Startup can therefore leave
completed repairs behind after a later I/O failure; the next startup selects
only the remaining open Turns. No Kernel is returned until those repairs finish,
and neither attempt replays external effects.

Authoritative format changes use explicit schema rejection. The
[Store opening decision](2026-09-01-agent-store-validation-and-read-snapshots.md)
requires read-only schema acceptance before writer configuration or staging
cleanup, so rejection preserves the old main file, WAL and staging payload.

## Alternatives considered

A current-tail lookup or timestamp comparison cannot reconstruct the selected
terminal's exact control prefix. Multiple terminal Facts in one Session append
would give only the last one an unambiguous final control position. Making the
flusher take submission admission would deadlock admitted writers waiting for
its durability barrier. Automatically rewriting stale business controls could
change accepted receipt identities and weaken their compare-and-append guard.

## Consequences

Store admission rejects missing, duplicate, orphan, mismatched and non-final
markers before mutation. Cold SQLite validation and offline audit compare
canonical markers with their derived indexes; hashing accompanies the existing
bounded control-decode pass, and the validation cache reuses that proof.

Gated Kernel tests cover both sides of Store apply, exact receipts after the
terminal, and failure between separate recovery repairs. Disabling the control
fence reproduces a receipt with the wrong sequence. Shared Memory/SQLite tests
prove a fork retains the terminal horizon despite later idle controls. File
preservation tests cover old and current schemas whose committed state remains
in WAL. Tests seed completed history through explicit correlated commits rather
than retaining a Fact-only compatibility path.
