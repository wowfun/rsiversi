# rsi-agent-program

This opt-in native runtime executes explicit JavaScript programs with shell-equivalent
Sandbox authority. Node is an ordinary confined Process, not a security VM. The
runtime receives a complete child environment; it does not inherit credentials or
Node startup hooks. Application business logic remains in its owning Rust modules.

One shared engine serves foreground program Tools and detached workflow owners.
Preparation freezes the script (64 KiB), RPC handler and exact confinement plan.
Foreground preparation retains the Tool cancellation token through the start latch;
cancellation during confinement prevents Jobs admission.
Jobs admission owns cancellation before a separate start latch can launch Node.
Dropping an unstarted admission cancels it. Jobs remain process-local; durable
acceptance, detached authority, budgets and recovery belong to Kernel.

The duplex protocol is big-endian length-prefixed JSON, at most 1 MiB per frame and
16 outstanding RPC requests. A single writer preserves frame boundaries. Process
owns termination and reaping. Closing RPC admission cancels admitted handlers
and immediately requests process termination. The runtime retains handlers and
pending process creation through their actual settlement before publishing its
terminal result. A non-cooperative handler can keep `result()` pending after the
process has terminated; retirement's bounded wait does not abandon that owner. The
serialized script result is at most 256 KiB; stderr has a bounded 64 KiB tail.
Job output preserves that tail's whole-stream offsets and reports discarded bytes.
Every admitted setup path finalizes its Jobs scope before attempting durable run
settlement, including when either cleanup or terminalization fails. Jobs reporting and cleanup precede durable terminal publication; a
cleanup failure becomes a failed run rather than hiding a committed success.
Foreground completion reports the Kernel's authoritative terminal
outcome; cancelled runs never expose a successful script value.

`run_code` is an Exclusive Coordinator and has a 600-second foreground deadline.
Its exact Executor extension supplies the eligible sealed Tool catalog. Scripts
call `await tools.call(name, arguments)` and return their curated JSON result.
Local Callable tools use the ordinary policy, approval, budget and durable effect
paths. Recursive programs, Agent controls, human interactions and Portable tools
are unavailable. Plan mode denies the coordinator before its start.

`run_workflow` accepts a script and foreground/background observation mode. The
foreground observation lasts 30 seconds by default (at most 60); reaching it
requests durable detachment. Background observation detaches before the
Node latch opens. Neither mode gives the independently owned run a total elapsed
deadline. A separate Jobs scope uses the Session and immutable run identities in the program namespace;
ordinary executor Jobs keep their Turn scope. Script, process, child cancellation
and final durable settlement stay owned after the Tool returns.
Dropping the foreground observation during setup does not cancel the transferred
single-use setup; it can still admit and settle a run. Creator cancellation remains
checked at Kernel start/detach gates before Node launches. Detach and creator-cancel
command observation has a five-second deadline, including queue admission, and
also observes authoritative completion. An unacknowledged command returns typed
`OutcomeUnknown`; its admitted operation remains owned rather than being retried
or declared settled.
An acknowledged detach refusal returns its failure immediately instead of
extending foreground observation to the run's full lifetime. The independent
owner continues to settle the run after that failure.
The Session protocol owns the typed invocation result: a detached return reports
running with an exact Session/run locator; a foreground return reports the
authoritative outcome and optional curated value. Producers and presentation
decoders share that contract.

Scripts call `await workflow.agent({message, output_schema?})`, sequential
`workflow.pipeline([async value => ...], input)`, concurrent
`workflow.parallel([async () => ...])`, and awaited `workflow.phase(name, value)` /
`workflow.log(value)`. Agent replies carry exact initial settlement metadata and
the complete verified structured value, or a bounded public reply when no schema
was requested. These calls cannot change the run's model, permission or fork
boundary. A registered, disabled plan-policy domain is required for creation;
any mutation of its captured revision revokes the run.
Initial human root Turns and finite built-in Goal/Schedule rounds may create one
run; child or completion Turns cannot create successors.

`workflow_read` returns run state, child counts, latest phase/progress, result
binding and curated result as revision-bound JSON fragments. The complete rendered
page, including metadata and the data label, defaults to 8 KiB and cannot exceed
16 KiB. Continue with both `offset` and `control_seq`; if progress changed the
revision, restart at offset zero. `workflow_cancel` revokes the same Session's run
and its owned child work while its live owner exists. If that owner is lost before
terminalization, cancellation reports stale authority; restart recovery interrupts
the orphaned run without replaying it. These are ordinary Tool cards on all native clients;
neither operation starts another run or grants access to another Session.
Known Turn-service and Tool cancellation, capacity and invalid request refusals
retain their Tool failure categories through preparation, setup, observation and
control. Ordinary script-visible RPC refusals remain catchable; an uncertain RPC
forces interruption of the independently owned run.

The Session protocol owns the outstanding-call, script and curated-result bounds.
The Node bootstrap receives its RPC capacity from the native start frame. The
executor permit covers queued and executing requests; the channel bounds queued
transport, and Context checks the durable active-effect bound during replay.
Runtime retirement cancels and retires its Jobs producer, then waits up to thirty
seconds for retained workflow owners. Timeout reports an error and leaves admitted
owners responsible for completion; it does not assert forced task termination.

Program results retain a typed `OutcomeUnknown` across Process, RPC and Jobs.
Definitions are pure descriptions frozen before Jobs admission; a definitions
panic refuses admission. An admitted RPC callback's construction or polling panic
is `OutcomeUnknown`. Execution and workflow completion owners retain single-use
operations and cleanup independently of observation, including on task unwind or
abort; a panicked operation is never polled or invoked again.
Recovery itself occupies the same transferable future slot. Aborting while
recovery awaits reporting or cleanup retains that exact future and completion
observers until settlement. Abort before durable acceptance refuses acceptance;
abort during an admitted method joins that method rather than reconstructing it.
An uncertain nested call is never returned as a catchable JavaScript error:
the engine cancels and joins admitted RPC owners, then interrupts its owning
Tool or workflow. A later cancellation or cleanup failure cannot turn uncertain
effects into confirmed cancellation or an ordinary retryable script failure.
Workflow setup, cancellation, reporting and terminal settlement preserve the same
typed uncertainty. An existing Kernel terminal remains authoritative, while an
uncertain observer receives no successful script value.
Abort-transfer settlement requires the Tokio runtime to remain alive. Abrupt
runtime destruction cannot guarantee completion of the retained cleanup.
Ordinary stderr read errors only lose diagnostic output. A panic at stderr capture
or process termination is a contained Process-boundary violation and keeps the
result uncertain; process-settlement failure also overrides provisional script
success. A known settlement failure preserves an earlier known execution failure;
uncertainty overrides either. A parsed script result alone does not establish
completed settlement.

Program Tools prepare the Node executable, complete environment and bootstrap argv before Approval.
Jobs admission accepts that move-only plan separately from the script/RPC owner;
the start latch consumes it once through the same execution tuple. A Local lease
seals the contribution's explicit Node configuration; SSH resolves the target's
`node` selector and environment. Native-only embeddings use that explicit
configuration through their supplied Process and Sandbox, using asynchronous
filesystem validation for the configured local executable. Preparation
never canonicalizes a remote program on the Service filesystem.

Workflow cleanup and Kernel Program publication have separate admission gates.
A known refusal before `ProgramRun::accept` commits creates no durable run and
therefore publishes no Program terminal. The owning Tool settles its typed refusal
through ordinary Kernel effect settlement. An uncertain acceptance is eligible
for terminal reconciliation; cleanup uncertainty never invents acceptance.
The Workflow owner's flags track retained cleanup operations, while the Kernel
reducer tracks canonical accepted-run events.

Script error frames retain at most 4 KiB of UTF-8 diagnostic bytes at the engine
boundary, without splitting a character. This bound is bytes, not characters.
