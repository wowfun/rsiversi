---
name: Native and application prompt preparation ownership
---

## Problem

Native Context concatenation cannot express DSH's scoped named providers, complete
sections, interpolation or runtime-context refresh. Adding an assembler before the
same implicit Header/persona prefix would duplicate defaults and make an empty
prompt ambiguous. Application and extension preparation also author receipt-bound
decisions whose callback order cannot be changed by a native presentation layer.

## Decision

The ordinary native prompt plugin is captured with the existing Agent generation. It assembles
after BeforeStep contributions and Tool restrictions, before final positional input
preparation and atomic Step publication. Preserve immediate native Plan enforcement
and commit-on-rejection. Distinguish complete assembly from append-to-Header output
with the tagged Session value defined by the
[Session protocol](../../../../crates/rsi-agent/session-protocol/README.md).

The selected application source owns mixed requests; extension presets omit native
assembly. Both retain their original preparation sequence. Reuse exact scope layers
and caller-owned effects. Keep prompt around continuations in Agent, without a new
generic Meta scheduler. Tool presentation order is separate from execution authority.

The authoring registry is generation-local. Fresh and restored compositions can be
active together, so the Standard catalog explicitly gives PromptRegistry its own
Local isolation in each generation. A shared registry slot would reject the second
provider and prevent the first restored Turn from starting. Other Host services
remain shared according to their existing ownership; isolation is an explicit
catalog declaration and participates in its identity.

## Alternatives considered

Inferring ownership from empty strings cannot distinguish clearing the whole prompt
from retaining Header defaults. Running two assemblers repeats provider work and
can cross JS receipt boundaries. Persisting registries or raw variables alongside
resolved output creates duplicate durable truth. Extending native Tool subsets to
mean ordered presentation conflates authority with model-facing order.

## Consequences

The Session format and Context builder identity change intentionally. Old durable
formats fail explicitly while preserving their bytes; no implicit reset or
compatibility interpretation is introduced. Provider retry reuses the accepted
assembly, and Context consumes its tagged result without prefixing defaults again.
Required runtime context remains present when optional context is suppressed.
Names also identify durable producers, so native declarations use the existing
bounded ContributionId grammar rather than DSH's arbitrary JavaScript strings.
Resolved prompts remain Step-local: unchanged model/Tool rounds within one Step
reuse the persisted text, but a new Step must publish its own resolved prompt.
Keeping a previous Step's prompt as active state would let omitted preparation
inherit expired instructions. Cross-Step payload references would instead require
a separate durable retention and replay contract. Keep the current self-contained
facts until measured cross-Step storage costs justify that machinery. The cost is
linear in actual Step boundaries: 100 Steps at 100 KiB add 9.765625 MiB of prompt
text, excluding fact framing and database overhead; 100 model requests need not
represent 100 Steps. A fully assembled prompt can reach 224 KiB.
Scoped callback lifetimes must not retain a strong Runtime cycle. Native-only
success cannot establish mixed compatibility, browser parity or physical-device
resource support; those require their own measured evidence.
