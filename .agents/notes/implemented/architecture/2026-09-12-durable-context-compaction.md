---
name: Durable model context compaction
---

## Problem

Evicting complete old Turns under byte/message pressure cannot reduce a long
active Turn and loses context without retaining a summary. A checkpoint is only
a cache, so a model summary cannot become authority there. A completed effect
without its successor must still interrupt recovery; treating a summary as an
ordinary assistant answer would both expose internal text and incorrectly finish
the Turn.

## Decision

Each ModelIntent freezes a typed Conversation or ContextCompaction purpose.
Compaction binds builder identity, Header, selected source spans/digests,
transitive prior summary, Usage horizon and output limits. The pure cursor plans
and validates; the executor performs the existing serial model effect with no
Tools. Only a valid durable Finished installs a summary, and replay makes
unsupported or out-of-selection summaries inert. Both Interrupted rules remain
in force. Model events also carry the small purpose kind, checked against their
intent by Kernel. Session API history can discard earlier Facts to fit its
encoded reply bound, and GUI/TUI projections accept partial historical pages.
Requiring an intent lookup for every output block would add an admission and
loading dependency just to distinguish internal summary text from an assistant
answer. The event tag preserves that distinction directly in each bounded page;
it does not duplicate the compaction plan or become independent install
authority.

Pressure uses reported successful Conversation input tokens at eighty percent of
the described model's context window minus default output reserve. Describe is
provider-I/O-free but does not prove the later preparation's generation. With no
applicable Usage, canonical hard limits and explicit ContextLimit recovery
apply. Instructions, original task input, latest human steering and whole recent
interaction units are protected, including a 64 KiB canonical-byte recent tail.

## Alternatives considered

Heuristic token estimates do not provide evidence about the active deployment.
Silent eviction loses task state. Running a model in contributions or cache
maintenance introduces an external effect outside serial admission. Installing
through a second Fact creates an avoidable crash window. Automatically replaying
after a durable summary weakens the existing uncertain-effect recovery rule.

## Consequences

Instruction protection follows the entered source contract. A replacement or
tombstone supersedes the same source's prior baseline, and a complete skill
catalog supersedes its earlier catalog. Superseded messages become summary
candidates; additive instructions and other sources remain protected. Retaining
every historical replacement would exhaust the message limit despite successful
summaries. Two 1,300-Turn replay/cache regressions cover ordinary small tasks and
repeated instruction/catalog replacement.

The [context contract](../../../../crates/rsi-agent/context/README.md) owns
planning and replay validity; the [executor
contract](../../../../crates/rsi-agent/executor/README.md) owns model effects
and bounded recovery.

Tests exercise exact Usage eligibility, hard limits, no-Tool summary requests,
strict canonical-byte shrink, bounded retry, invalid outputs,
budget/cancellation, durability windows, builder mismatch and exact fork
selection. A recovered summary is reusable by a new explicitly resumed Turn; its
original interrupted Turn never silently continues. Old cache formats fall back
to Facts.

Summary quality remains model-dependent. Raw Facts remain authoritative and the
summary is explicitly attributed internal context. Large unsplittable units may
not fit; failure must be explicit. Compaction consumes the existing Turn
provider and generated-record budgets rather than obtaining a separate budget.

Cold reconstruction is bounded to 4,096 projected messages and 32 MiB, with a separate
metadata bound. Retaining raw Facts does not imply that arbitrary history fits
in one materialized view. Scripted Session API pressure scenarios prove actual
no-Tool summary requests followed by summary reuse, independently of model
quality and live-provider coding acceptance.

The current builder retains direct source bindings only for materialized Turns and
the exact installed prior summary. Inductive replay validates each prior before
its successor, including fork visibility. Copying every transitive raw Turn
binding imposed a lifetime cap unrelated to the retained context. Fully
summarized completed Turns are released; the optional recent tail respects both
byte and message pressure. The private fold payload is version 7; the generic
builder envelope remains version 6.

An interrupted Tool batch is evidence that cannot be summarized as a completed
interaction. Protecting it permits unrelated complete history to shrink without
inventing results. Source-pressure admission and cold retention have different
purposes: a skipped model attempt must not consume the only chance to compact.
The [Context contract](../../../../crates/rsi-agent/context/README.md) separates
the planning batch ceiling from finite cold replay headroom. Silent raw-history
eviction is rejected because it would hide evidence without a validated summary.
Encoded source bindings can fill the plan before its item ceilings. Selection
therefore budgets compact JSON bytes, leaving metadata headroom and stopping at
a whole unit. Raising the durable bound would increase every model-intent
consumer's retention cost. Very large retained histories can require multiple
successful pressure events; the provider retry bound is not widened to hide that
cost. Long-identifier tests prove each install makes progress and replays exactly.

Provider Usage includes fixed overhead that selected history cannot remove, so
an empty optional selection must not prevent an otherwise valid ordinary call.
Quoted history also has a separate encoding boundary: plan metadata fitting
does not prove its no-Tool request fits. Selection accounts for that request's
JSON string escaping and prior summary before dispatch. An installed summary
consumes the preceding Usage observation even when inherited through a fork;
missing new Usage cannot repeatedly charge summary attempts against stale
parent pressure. The current limits and unsplittable-unit behavior remain owned
by the Context contract above.
