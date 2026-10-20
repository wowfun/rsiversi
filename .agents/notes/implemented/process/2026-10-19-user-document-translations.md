---
name: Explicit locales for user-document translations
---

## Problem

The generic Chinese suffix does not identify the written locale, and storing
translations beside English tutorials mixes the primary reading path with
localized copies. Moving translations must preserve the existing checks for
reciprocal links and identical executable examples.

## Decision

Use the explicit Simplified Chinese locale in language labels and translation
locations. Root README translations stay beside their English entry point and
include the locale in their filenames. User tutorial translations live under the
dedicated locale subtree and retain their English filenames. The
[documentation policy](../../../../crates/tools/rsi-xtask/README.md#documentation-policy)
owns the exact naming and path contract.

This refines the translation placement in the
[source-onboarding decision](../architecture/2026-10-16-source-onboarding-and-windows-applications.md).
Verification maps each locale-relative topic path to its English source in
both directions and reports misplaced translations rather than treating them
as English pages. Homepage command examples reuse tutorial-owned IDs so useful
front-door snippets cannot drift together in both languages while the source
tutorial changes. Product introductions remain summaries linking to their owners.

## Alternatives considered

Keeping sibling translations retains the mixed directory layout. Repeating the
locale in every tutorial filename duplicates its directory identity and makes
source-to-translation mapping more complex. Compatibility copies would create
competing documents and stale links.

## Consequences

Moves require repairing inbound links, links between translated pages and relative
links to owning contracts. Focused tool tests cover nested pairs, orphaned or
misplaced translations, reciprocal navigation and command parity without using
real user state or executing installation commands.
