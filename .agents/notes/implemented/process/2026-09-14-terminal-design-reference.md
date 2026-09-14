---
name: One current terminal design reference with independent decisions
---

## Problem

Interaction rules had accumulated in the product README, presentation README and
development guide. Repeating them during a redesign creates conflicting owners.
Combining unrelated protocol and UI changes into one proposal also prevents each
decision from completing or being reconsidered independently.

## Decision

The terminal subtree owns one [interaction reference](../../../../crates/rsi/terminal/docs/tui-design.md).
Product documentation links to it; terminal and presentation READMEs retain their
own lifecycle, trust, codec and resource contracts. The development guide owns
how to implement and verify a change. Durable rationale follows independent Agent
Notes for model selection, Tool origin, state settlement, metrics, evidence,
pricing, live preview and layout rather than duplicating those contracts.

This follows the ownership separation demonstrated by pinned DeepSeek Harness
`docs/AGENTS.md`, `docs/web-styling.md` and `packages/client/AGENTS.md`: current
references belong to the relevant subtree, facts have one authoritative home,
and Agent Notes preserve alternatives and evidence. It does not infer a universal
DSH filename or require a new documentation index.

## Alternatives considered

Keeping parallel specifications in READMEs makes routine updates ambiguous.
An omnibus design note gives independent changes a single lifecycle. A generated
navigation inventory adds maintenance without establishing a contract owner.

## Consequences

A behavior change updates its owning reference before implementation. A decision
change updates its owning note or records narrow supersession without rewriting
an implemented note into its opposite. `cargo xtask verify-docs` checks the
repository documentation and Agent Notes structure. Temporary test failures,
visual captures, live results and environment-specific limitations remain in the
requested local evidence record rather than becoming duplicated product rules.
