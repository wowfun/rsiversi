---
name: Plan mode composes through ordinary Agent contributions
---

## Problem

Planning needs shared draft and durable state across applications. A named branch
in the Kernel or a UI-only toggle would bypass generation pinning, canonical
command receipts and the policy that actually admits Tool execution.

## Decision

The ordinary [plan-policy plugin](../../../../crates/rsi-agent/plan-policy/README.md)
registers one typed domain and independent command, context, Tool-policy and
projection contributions through the existing Agent registrars. The standard
preset selects that factory through the same addon catalog as custom presets.
Session command admission and complete projections carry its values to consumers;
neither Kernel nor a wire adapter recognizes its name as a special execution path.

The mode starts disabled. The approved RSI allowlist adds constraints to prepared
Tools without relaxing sandbox or approval requirements. Commands propose only
typed replacements and inherit existing exact retry, revision, draft publication,
preset reset and fork contracts. Context records current mode before provider I/O,
including deactivation after earlier planning instructions. The existing terminal
policy-denial contract remains explicit: rejection records provenance without an
intent/start or fabricated Tool result and ends that Turn.

## Alternatives considered

DSH's planning plugin supplies useful command/state/context separation, but its
advisory mode keeps all Tools executable. RSI's approved allowlist is a deliberate
product requirement. Classifying arbitrary shell command strings as read-only
would create a second incomplete sandbox. Exact declared Tool names keep policy
simple and let the authenticated Files capability supply bounded readers later.

## Consequences

The plugin is removable or configurable by an Agent preset; it owns no global
registry, Store writer or UI state. Changing a preset's domain catalog still uses
strict cold-generation support checks, without a compatibility shim. Tests use a
real Meta/Agent generation, actual Kernel state, and the standard product's
provider request and Tool-denial paths. Native interactive command affordances
remain consumers of the generic Session command API.
