---
name: Separate claimed input from prepared model input
---

## Problem

DSH claims inbox input, assembles the current prompt and runs the pre-step
waterfall before entering the returned messages. Claude Code Mods can rewrite
several human messages, leave later message content empty, or reject the proposal.
RSI immediately projects mailbox input and direct Turn text, so an application-only
rewrite would disagree with Context and the immutable application journal.

## Proposal

Keep acceptance, claim and final model entry distinct. A native preparation owns
the exact claimed input horizon, final ordered decision and staged domain changes.
Only its acknowledged durable publication enters model history. Provider retries
reuse it; uncertain publication never replays a callback. Direct Turns, mailbox
input, steering, preset context and deferred tool context use the same boundary.
Consumed input is not requeued by an empty or rejected decision.

Represent unchanged final entries by local Fact/ordinal coordinates. Replacements
retain that coordinate plus the new attributed message; added messages have no
claim coordinate. Preserve list order and repeated coordinates as presentation,
without duplicating mailbox consumption. A captured owner resolves coordinates;
metadata cannot confer provenance or execution authority. Reject and an empty
enter remain separate outcomes. Empty message bodies produced by a rewrite remain
valid journal data and are normalized at model projection, not rejected as new
ingress. Reuse the ordinary message/block and durable byte boundaries; do not add
a second pending queue, JSON hash gate or per-message RPC.

Preparation belongs to a provider request series, not the native Step lifetime.
A native Step may contain several model/Tool rounds; retries reuse one captured
decision. An application capture retains its scoped Agent, resolved tools, prompt
assembly and input callback, including when no Tools are selected. Rejection has
the explicit Turn outcome `Blocked`; an initially empty entered list completes
without a provider effect. Native guidance is outside DSH's user-message waterfall
and stays in its original slots, except when all claimed user input is removed.

The Kernel claims settled Tool contexts before capturing the standalone hook's
history. This gives the waterfall stable input coordinates and lets the existing
history reader carry original messages across retries and process replacement.
The native fold retains unclaimed results across checkpoints and forks. A
separate pending-message channel would repeat the payload and require its own
pagination and lifetime handling.

The standalone Cordis process can reuse its acknowledged Session mirror between
callbacks. The native Store owns one immutable domain snapshot per positive
revision within that Session. Its revision and immutable header identify the
retained state after reconciliation. Rebuilding the Session every time would
also invalidate plugin caches keyed by Session identity. Revision-zero drafts
can change under the same revision and need full transfer. A new process or an
eviction also requires restoration from the authenticated journal.
Within one incarnation, the append-only event log and its acknowledged length
also identify the committed prefix. A receipt can publish its staged projection
without copying the prefix or evaluating the projection a second time.

The DSH projection checkpoint retains pending Fact coordinates and message counts,
not another copy of input bodies. Live projection branches share their captured
immutable Facts. Cold recovery resolves each needed Fact once; replacement and
omission need no original body read. This keeps the bounded extension checkpoint
independent of large valid inputs without introducing a second durable body store.

Complete activation requires replacing early-entry Facts, updating claim control anchors, Context checkpoints,
Store validation/reference walking, Kernel preparation admission, both external
adapters and history projection together. Exact format/schema versions advance
at that activation; no old-format fallback is proposed. Partial native and bridge
tests do not establish complete activation until those paths are connected and
the same original plugins run through a real Host.

Each preset edits its predecessor's decision. The final application sees that
result plus the staged context. The final decision and domain updates share
one publication so a failed later editor cannot leave an earlier journal
committed. A new context claim in that publication uses its distance from the
decision Fact. This preserves its identity across intervening writers without
holding a Store lock during plugin execution. Previously captured claims retain
absolute coordinates. Sending only the unchanged claims to the next process
avoids rereading discarded bodies. Ordinary decisions fit the stage reply.
Large decisions use scoped fragments so valid edits can cross the same boundary
without increasing every process frame. A committed rejection ends the Turn
even if its observer loses confirmation.

A tool-only application can disappear without changing which input the user
authorized. An application that participates in input preparation can reject or
rewrite that input. A cold Host demonstrated the difference when its tools-only
fallback sent the original prompt and continued through 23 model intents.
The [source contract](../../../../crates/rsi-agent/composition-protocol/README.md)
therefore distinguishes those roles. Waiting for the participating source
before starting the Step clock keeps a slow first launch cancellable without
replaying the original input or delaying unrelated Host operations.

## Alternatives considered

Rewriting only the provider request preserves a different recovery history.
Appending replacement user messages retains the original unwanted instruction.
Mutating old journal rows violates the accepted-prefix contract. Repeating all
unchanged text in the preparation record adds avoidable durable bytes and cloning.
Replaying a callback after a missing acknowledgement can duplicate plugin effects.
Keeping claim and entry as one operation excludes DSH's drop/reject semantics.

## Acceptance criteria

Original Claude Code Mods rewrites, rejects and appends context through the market
Fiber on standard native Sessions. A rejected or initially empty entered list
spends no provider request. Direct input, initial mailbox batches, steering and
tool-supplied context share the semantics. Reordering and replacement survive
provider retries, checkpoint restore, selected fork history and cold recovery.
Exact claim/tool-receipt consumption remains provable even when no message enters.
Native request evidence and original DSH history contain the same final content.
Lost publication acknowledgements do not rerun hooks. Targeted tests, original
plugin runs and live/visual verification prove these paths before completion.

## Risks

Claim coordinates must be resolved within their captured source Session and
input set, including fork windows. New references in rewritten content need the
same resource retention as other durable input. An empty message differs from
an empty final list. Rejected preparation must publish staged plugin state and
its outcome without starting a provider effect or losing the source claim.
