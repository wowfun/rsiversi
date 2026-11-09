---
name: Preserve native Sandbox failures through Execution
---

## Problem

Native Tool preparation through an Execution lease discarded the requested
unsupported sandbox mode, then replaced the remaining process error with an
unavailable-admission message. The same planner used directly by a native Tool
already returned its typed Sandbox failure. On Windows this concealed the
missing restricted backend before approval or child creation.
The native root check also tried to canonicalize `/` on Windows, rejecting a
valid workspace before backend selection. A test that only required any error
had concealed this earlier failure.

## Decision

The local Execution adapter carries non-capacity planning failures through
`ProcessError::Sandbox`; Tools restores the existing `ToolError::Sandbox`.
Sandbox capacity remains the Process capacity category. The current contract
belongs to [Process](../../../../crates/rsi-process/core/README.md).
The planner rejects a filesystem root by checking the already canonical
workspace's parent, covering native Windows roots without another filesystem
query. Tests distinguish a valid directory with an unavailable backend from
an invalid root.

## Alternatives considered

Adding a Windows-specific message at the UI would miss other planning failures
and duplicate knowledge of the execution backend. Printing all process errors
at the Tool boundary would also change unrelated remote and admission
diagnostics. Typed propagation uses the existing direct-native error contract.

## Consequences

The caller receives the mode and actionable planner cause before any process
starts. This changes neither sandbox selection nor approval, and does not
provide a Windows restricted backend. Remote transport error policies remain
with their transport owners. Capacity, retirement and unknown-outcome errors
keep their separate semantics.
