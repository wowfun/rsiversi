# rsi-apply-patch

This package is the ordinary Linux apply-patch plugin. `ApplyPatchToolFactory`
requires an explicit canonical helper executable and registers only the
`apply_patch` definition through `ToolRegistrarContract`. Tool execution
preflights the bounded patch before confinement, invokes the helper through
Process with the exact sole marker `--rsi-run-as-apply-patch`, joins it even
after cooperative cancellation, and accepts exactly one bounded JSON line with
internally consistent status/effect metadata.
Once the helper has started, cancellation may race a committed filesystem
prefix before the helper can emit its sole effect ledger. After terminating and
reaping that helper, the Tool therefore returns an explicit `effects_unknown`,
`effects_known: false`, non-replayable result instead of a bare cancellation.

The private patch engine owns no-follow, descriptor-relative filesystem
resolution, complete preflight, bounded fuzzy-match audits, and exact partial
effect reporting. Fuzzy audit and prospective-directory budgets are enforced
while preflight discovers them, before another path string is retained. Only
directories absent during preflight are prospective `mkdir` effects; commit
must not create an unplanned directory if the tree changes before mutation. A
newly added regular file starts from mode `0644` filtered
through the helper process's Linux umask; updates and moves preserve the
preflighted source mode. Each update lazily derives its rstrip, trim, and Unicode
source views at most once, so additional hunks do not repeatedly normalize the
whole target file. A mutating helper invocation is never replayed automatically.
Descriptor-relative revalidation narrows, but cannot remove, the final
same-host race between validation and `renameat`/`unlinkat`; exact partial
effects remain the recovery authority if a concurrent actor mutates the same
directory at that boundary.
The public helper dispatcher reads stdin only for the exact sole marker so a
host executable can route normal CLI invocations first. Both activation and
helper execution fail closed outside Linux until equivalent filesystem
semantics exist.
Patch framing normalizes CRLF delimiters but preserves a lone carriage return
inside an added line. A pure-addition update with an `@@ context` marker inserts
after that exact anchor; without an anchor it appends.
Updated text is serialized with the source's first observed line ending and a
trailing line ending, so mixed endings are intentionally normalized. An
`*** End of File` hunk is anchored to the final matching block. If its expected
lines do not end the source, preflight rejects the patch instead of silently
applying the hunk to an earlier occurrence.
`delta_exact` means the returned filesystem effect ledger has exact before/after
byte counts; fuzzy source matching is reported independently in
`fuzzy_matches` and does not make that ledger inexact.
