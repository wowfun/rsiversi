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
owns termination and reaping. Cancellation closes RPC admission and cancels and
joins admitted handlers before the runtime publishes its terminal result. The
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
records detachment, not cancellation. Background observation detaches before the
Node latch opens. Neither mode gives the independently owned run a total elapsed
deadline. A separate Jobs scope uses the Session and immutable run identities in the program namespace;
ordinary executor Jobs keep their Turn scope. Script, process, child cancellation
and final durable settlement stay owned after the Tool returns.

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

The Session protocol owns the outstanding-call, script and curated-result bounds.
The Node bootstrap receives its RPC capacity from the native start frame. The
executor permit covers queued and executing requests; the channel bounds queued
transport, and Context checks the durable active-effect bound during replay.
Runtime retirement cancels and retires its Jobs producer, then waits up to thirty
seconds for retained workflow owners. Timeout reports an error and leaves admitted
owners responsible for completion; it does not assert forced task termination.
