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

DSH's inbox claim drains next-Step input before taking the waking next-Turn input.
Applying that order at native claim admission avoids an empty first Step and
keeps the pre-step waterfall's input order independent of message provenance.
Sorting all sources together by acceptance time is insufficient: a held Step
message may arrive after the queued follow-up that eventually wakes it.

A bounded Store mailbox page remains the admission unit; this ordering change
does not remove its aggregate byte limit or establish parity for a queue spanning
several pages. That case needs a separate claim/publication design, rather than
unbounded materialization on small devices.
The claim page selects held Step inputs and the waking message's command batch
before loading payloads. Unrelated queued Turns cannot consume that budget or
hide later held input. This follows DSH's separate queue claim while retaining
RSI's atomic publication and finite admission bound. Ordinary-input precedence
over continuations comes from indexed metadata in the same Store snapshot;
it must not depend on which bodies fit in a page. No extra body index is needed.

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

### Accepted input body ownership

The accepted control owns the immutable mailbox message. Neither the mailbox
index nor the queued claim needs to persist that content and attribution again.
DSH also appends inbox messages into its model-visible event stream,
so its current JSONL representation is not itself a durable deduplication scheme.

Queued claim bodies are normalized at the Store boundary while retaining the
logical Fact contract for consumers. The physical claim identifies its
same-Session acceptance; one bounded joined read can decode both records into
the typed logical input without creating an intermediate expanded JSON document.
Inline contribution claims without an acceptance remain self-contained. The
original typed append is compared with the accepted payload before removing
any body: validating only a rehydrated row could hide a conflicting submitted
claim. The current append already owns those typed values and need not reread
the rows it just inserted.

All Fact readers share this reconstruction, including cold recovery, selected
forks and application history. Indexed logical lengths preserve read budgets;
bounded physical projections govern allocation. Prefix fingerprints remain
logical while reference coverage hashes the stored bytes and retains logical
edges. Fact retirement precedes acceptance retirement. Schema 46 rejects earlier
physical encodings without touching them; this pre-release compatibility choice
avoids a migration branch, but migration remains technically possible.

A paired read experiment exposed the cost of constructing and counting a full
control before constructing and counting the resulting Fact. The Fact decoder
therefore reads the acceptance identity and message projection and validates the
logical Fact once. Canonical control replay still validates the entire acceptance,
including routing and options. This keeps validation at its owning boundary
without a second body cache or an expanded JSON representation.

## Alternatives considered

Rewriting only the provider request preserves a different recovery history.
Appending replacement user messages retains the original unwanted instruction.
Mutating old journal rows violates the accepted-prefix contract. Repeating all
unchanged text in the preparation record adds avoidable durable bytes and cloning.
Replaying a callback after a missing acknowledgement can duplicate plugin effects.
Keeping claim and entry as one operation excludes DSH's drop/reject semantics.
An expanded SQLite JSON view would conceal the physical distinction, but must
parse and serialize the accepted payload before Rust parses it again. A local
large-input probe showed a substantial read-cost increase. Returning bounded raw
records for one typed decoder avoids that extra representation. Making every
Context, fork and browser consumer resolve mailbox references separately would
spread a mechanical storage concern into the product state machine.

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
