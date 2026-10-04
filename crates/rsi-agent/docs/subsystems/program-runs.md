# Program runs

The opt-in [Program runtime](../../program/README.md) owns Node execution through
Process duplex and Jobs. Kernel owns the separate durable ProgramRun lifecycle;
Jobs remain process-local. [Execution ownership](../../kernel/README.md) is distinct
from immutable fork lineage, so a detached run retains a frozen composition and
policy without retaining a retired Tool claim. Its initial child completions
settle into run receipts. A bounded generation-bound notice publishes only the
curated completion back to the Session. Startup interrupts unfinished runs and
discards their unclaimed work and old notices without replaying external effects.

## Session workbench reads and cancellation

Trusted Session adapters use separate Local Kernel list/read/cancel operations;
model operations still require the original live Agent caller. History is ordered
by canonical acceptance sequence within one Session and a fixed watermark. Kernel
alone folds lifecycle controls; clients refresh snapshots on invalidation.
Cancellation acknowledges durable acceptance, not completed cleanup. Terminal
retries return the existing outcome; a nonterminal run without a live owner is
explicitly orphaned until startup recovery. Unknown acknowledgements are recovered
by reading the same run, never by creating a replacement.
Preparing an explicit Session cancellation is read-only. Preparation failures and
known commit refusals leave its live signal untouched. A successful commit or an
uncertain admitted commit stops the run; the latter returns
`ExecutionOutcomeUnknown`. Creator cancellation instead revokes the creator's
remaining execution authority immediately unless detachment already won; Store
refusal cannot restore that authority. Owning Session admission serializes these
decisions with terminal publication through commit completion.
CAS reads acquire byte admission in the actual worker before allocating and
retain it through the last returned byte owner.
