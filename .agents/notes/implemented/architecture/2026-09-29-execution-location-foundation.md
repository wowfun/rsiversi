---
name: Location-bound execution authority
---

## Problem

Independent consumer hashes of native paths cannot identify another machine.
Sandbox prepares resources that Process consumes, so routing those providers
independently cannot safely preserve their authority. Tool argument hashes alone
do not identify the execution location. Execution also depends on the
[Storage authority contract](../../implemented/bug-fix/2026-09-28-storage-commit-authority.md).

## Decision

`rsi-execution/protocol` owns validated location coordinates; Workspace owns the single identity derivation.
An ExecutionLease pins Sandbox, Process, Duplex, PTY, Files, target path/program
resolution and environment policy together for each execution. Prepared plans are opaque and
accepted only by their matching provider generation. Approval execution binding
includes the target, workspace, policy and prepared-plan identity.

Executor claim admission covers Jobs creation; PTY admission covers terminal
creation and resize. Jobs itself remains unaware of Execution leases.
Accepted operations retain ownership after a caller drops. Start admission settles
at verified handle consumption or unpublished-child cleanup; long-lived processes
retain a separate lease pin and cannot hold a completed start operation open. Native
plan resources release at actual settlement independently of retained output capture. Unknown
effect outcomes propagate through Tools into interrupted Turns without automatic replay.

The pre-release format transition requires a new state directory and leaves the
old directory untouched. It includes canonical workspace and activity indexes,
the final frozen MCP reader contract and request-time Context image projection.
The feature implementations share those contracts.

Execution evidence includes the existing Host epoch in addition to target and
process-local provider, lease and plan counters. Counters reset when the Service
restarts; persisting them without a lifetime namespace could make an old approval
look like evidence for a new plan. The Host owner supplies that epoch, while the
opaque in-process lease seal remains the execution authority.

Trusted Workspace and Session ingress must retain the authenticated caller rather
than dispatching through their unscoped Local services. Shared draft preparation
does not imply shared caller authority. Reconciled views share the draft state
while each retains and rechecks its own caller. Finite metadata and live stream
frames use admission independently of target connectivity.

The Kernel retains execution leases per pending input and accepted Turn. A
Session-wide last-caller slot would let a later user replace the authority of
already accepted input; durable coordinates would resurrect authority after
restart. Neither is a valid source for claim execution. Pending input without
its live lease waits for explicit authorized resubmission of that exact input.

## Alternatives considered

Adding a location parameter to each provider independently permits mismatched
Sandbox and Process authority. Encoding an SSH destination in a native path
confuses identity, filesystem interpretation and authorization. Runtime format
compatibility branches would preserve obsolete execution contracts.

## Project context across machines

The existing workspace source owns ancestor instruction selection, skill directory
links, metadata/body identity checks and last-good publication. General Files
workspace scopes intentionally cannot read ancestor roots. Extending ordinary file
Tool authority to every instruction root would conflate those policies.

A fixed helper application entry therefore reuses the workspace source's bounded
project collector under the selected lease's ReadOnly process plan. Reusable SSH
transport remains unaware of Session and context state. Only the application links
the collector. The Service receives structured project sections and selections,
then adds its configured user sources using the same precedence and aggregate scan
budget. No Service user text or configuration is sent to the target. This also
avoids concatenating independently truncated catalogs or parsing rendered prompts
back into a selection model.

DSH's `context/agent-instructions/src/files.ts` accepts the selected FileSystem in
`findProjectRoot` and bounded source reads, and its plugin passes `ctx.fs` at the
request boundary. RSI keeps its stronger incomplete-observation rule and native
handle-based source checks. The helper entry is an RSI implementation choice; the
reference does not establish its sandbox, memory or grant guarantees.

The first native SSH collector experiment exposed two mechanical constraints:
Process batch tails retain at most 4 MiB, and ordinary Bubblewrap plans hide host
`/tmp` before rebinding the workspace. A nested workspace under `/tmp` therefore
lost its real Git boundary and ancestor instructions. Moving the fixture would
hide a supported-path defect. The collector now consumes a bounded Duplex stream,
and Sandbox exposes a distinct ReadOnly pipe source view with read-only host
scratch and isolated networking. Trusted source owners select that view: the
helper's fixed collector entry and workspace Git review. This is not a wire-level
allowlist of executable names. Ordinary Tool and PTY plans preserve their prior scratch
semantics, and PTY validation explicitly excludes the source view. The adapter
requires source-view evidence before starting, so an ordinary plan cannot publish
a misleading complete baseline.


## Files token authority

A new API request acquires a new caller lease. Keeping Files caller mappings only
inside that ephemeral lease would invalidate every returned token at request end;
keeping the opening lease instead would lend the opening caller's grant to later
requests. Execution therefore separates a retained private Files scope from its
current operation view, as it does for PTY. Exact provider-object comparison
rejects reconnect substitution before backend I/O. Endpoint token entries bind
principal and Session/Header and retain at most 64 resources, including accepted
opens and queued unpublished replies. Cleanup does not require a new target
connection. Files resources do not retain Session draft activity.
Read or listing failure alone cannot prove token loss: an unavailable I/O object
can still have a live description. The endpoint rechecks that description before
releasing its resource owner. A retired SSH connection reports cancellation;
its old tokens cannot move to a replacement provider generation.
The [Session Files contract](../../../../crates/rsi/session-files/README.md) owns
endpoint-token translation. Provider counters are local to independent processes,
so sharing their raw tokens would let another provider occupy an unrelated owner's
correlation value. Endpoint-owned identities separate routing from provider naming
without adding provider identity or authority to the wire.
Duplex supervisor recovery retains the native child and unfinished pipe joins
together. Waking waiters alone cannot justify releasing the confined plan: the
resource receipt comes from actual reaping and joins. A failed recovery fences
admission and retains the unsettled reservation, following the
[local Process contract](../../../../crates/rsi-process/local/README.md).

History's API adapter previously discarded its origin while its Tool path called
the same unscoped Service owner. Both now carry explicit live authority into the
finite worker: API origin admits offline location access, and Agent authority
narrows the request to its exact coordinates and original execution lease. The
rebuildable cache and source coordinates cannot substitute for either authority.

## Consequences

Files authority includes the execution lease even when application caller,
Session revision and workspace spelling are equal. Each lease scopes incoming
Files callers to private opaque identities; its views share those mappings, while
other leases cannot read or release their tokens. Final lease release withdraws
remaining private callers after retained operation pins have drained.

Cross-location and cross-generation prepared plans perform no I/O. Local
execution passes its existing behavior tests through the same pin interface.
Unknown started effects end as interrupted and are not replayed. Workspace
identity is derived in one owner, including identical paths at distinct targets.
All persistent consumers honor Storage fencing and accepted-operation lifetime.

The asynchronous change crosses several producer contracts. The format transition
includes `Capture::configuration` in durable ModelIntent evidence bytes and their
digest. It is deliberately incompatible; it must never reset or modify an existing state
directory implicitly. Frozen catalogs and Context identities must be tested
across the later implementation stages, not merely assigned future version numbers.

Local Tool contributions already own independently frozen executable/environment
configuration (Bash, Apply-Patch and Program). Replacing it with one application
catalog would silently change a configured Program runtime. The Local lease therefore
admits and seals that explicit configuration through its pinned native
resolver. The corresponding operation rejects SSH before backend I/O; SSH Tools
continue to resolve target-side named policy. This retains complete tuple ownership
without copying Service environment or rediscovering a replacement provider.


A retained PTY cannot keep borrowing its creator's grant for later callers: that
would prevent another authorized attachment from controlling it after creator
revocation. Its private bounded output projection and cleanup therefore belong
to an execution resource, while write/resize use independently admitted views
of the exact original provider. Those views must not construct another managed
process owner: the last-owner destructor would terminate the resource after one
input. Releasing a view releases only its pin; the Session scope owns termination.
