---
name: Bounded operation evidence and live GUI ownership
---

## Problem

The apply-patch helper owns the only exact preflight bytes and committed effect
ledger. Sharing presentation evidence with model content would repeat large
diffs in context. Opening every GUI renderer eagerly would retain hidden reads.
A filesystem reread cannot prove what an earlier operation changed, and a Jobs
scope acquired by a viewer would have authority to replace a finalized scope
generation.

## Decision

The Tool persists bounded optional operation diffs in Tool values and keeps
model content to the complete ledger. It reserves evidence from the frozen Turn
byte allowance, with deterministic whole-hunk omission and no filesystem reread.
Unknown helper outcomes remain explicitly non-replayable. Visible inline blocks
use the existing presentation owner and bounded source-reader mechanisms.
Current Turn Jobs observation shares the executor's exact scope through a
read-only service; it never acquires, reports, waits on or cancels Jobs. Durable
Goal state stays separate from the Host controller's live authority.

## Alternatives considered

Git or before/after snapshots would require a distinct attribution and artifact
lifecycle. Sending diffs in model content would charge repeated context for
presentation data. Making every renderer asynchronous would hide admission and
cleanup inside plugin callbacks instead of the GUI owner. Durable Jobs require a
separate recovery design and are outside current-Turn status observation.

## Consequences

The [apply-patch contract](../../../../crates/rsi-apply-patch/core/README.md),
[GUI owner](../../../../crates/rsi/gui/README.md) and [Session Jobs
boundary](../../../../crates/rsi/session-protocol/README.md) own the
corresponding current limits and interfaces.

Engine tests cover exact and fuzzy bytes and partial moves. Public Tool Runtime
tests cover helper failure, malformed arguments, encoded-byte limits and content
separation; product fixtures also execute the built helper. Deterministic presentation
tests cover source paging, stale completion, plugin withdrawal and retained
capacity. Browser and Linux desktop fixtures exercise actual inline cards and
Goal/Jobs controls; browser geometry checks include containment and hit testing.
Jobs observation does not change report state and becomes unavailable when its
original Turn scope ends.

Optional data still consumes durable record capacity. The executor supplies a
bounded allowance and the helper validates worst-case partial metadata before
mutation. A complete replacement hunk may be too large to display even when
individual edits are small; explicit omission preserves the exact ledger.

Two Session controllers require eight streams for Facts, interactions,
projections and Goal observation; renderer offers consume another. Client and
device subscription admission is sixteen, under the existing global sixty-four
limit and unchanged byte pools. Keeping eight would starve an existing pane when
optional Goal observation starts. This is bounded capacity for the shipped
composition, not an unbounded observer registry.
