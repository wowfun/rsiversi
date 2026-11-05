---
name: Process extensions retain proposals until canonical publication
---

## Problem

An external plugin can mutate its in-memory projection before RSI publishes the
corresponding ToolResult. Treating a successful RPC or Tool runtime retirement as
durability loses atomicity: retirement also occurs during cleanup. Reinvoking the
plugin after a lost acknowledgement can duplicate effects. A synchronous native
callback gate cannot carry independent reverse requests while another callback
waits for human input.

## Decision

The generic [process connection](../../../../crates/rsi-extension/core/README.md)
owns bounded duplex framing, generation identity, cancellation, native settlement
and a liveness watchdog. It runs explicitly trusted native-equivalent code. The
optional [Agent adapter](../../../../crates/rsi-extension/agent/README.md) owns
retained Tool settlement over the Agent's existing atomic domain commit.
Meta has no knowledge of extension event types or DSH services.

The published Better Sidebar resolves explicit filesystem paths with the
host user's permissions, including paths outside the Session's default
directory. A marketplace can also install executable code. Applying a path
filter to individual requests would leave other plugin operations with the
same privileges. The [application forwarder](../../../../crates/rsi-api/http/README.md)
therefore admits the browser identity issued by the local launch owner before
calling the plugin. Ordinary paired devices keep their existing API grants.

A resident listener keeps that launch identity across application crashes.
Each process owns its own key, endpoint and physical request handles. A new
request can wait for replacement while a request already sent to the failed
process keeps its original outcome. This lets the browser recover without
giving the replacement authority over an old mutation, model observer or
terminal writer.

Pure settlement produces typed proposals and retains a separate publication
observer. Executor verifies the exact canonical result and domain receipt before
notifying that observer. Dropping an unfinished notification preserves uncertainty.
The adapter reconciles exact receipts; absence licenses rollback only after the
originating Turn's terminal fence and a second request lookup. No path invokes the
Tool again to establish what happened. The
[composition contract](../../../../crates/rsi-agent/composition-protocol/README.md)
owns this interface, preserving the atomic publication decision in
[execution contributions](2026-09-08-execution-contributions.md).

The built-in Session journal envelope has explicit CAS and Session roots so
offline Store reference maintenance does not require an installed plugin. Its
event and checkpoint semantics remain extension-owned. It uses ordinary typed
domain proposals; it does not restore arbitrary JSON writers rejected by
[typed domain commits](2026-09-08-typed-domain-commits.md). A projection reads the
committed domain checkpoint without I/O. This is extension state, not a framework
projection cache; [pure projection ownership](2026-09-09-session-extension-projections.md)
is unchanged.

BeforeStep retains the existing atomic Step acceptance. One RSI durable Step may
contain several provider retry series, so preparation may replace current system
guidance and narrow Tool declarations after outstanding effects and calls settle.
The exact assembly is a durable `StepPrepared` Fact; a checkpoint retains it only
while that Step is open. Tool origins retain the selection of their producing
request, including nested Program dispatch. Historical PluginContext messages
cannot represent current Plan instructions: they would keep an exited policy in
later requests. This refines the execution-contribution representation without
sampling unrecorded guidance inside the Context builder.

Operation-scoped history reads borrow the captured claim-visible reader. Their
consumed cursor is extension checkpoint data and advances with the journal's
atomic publication. UTF-8 fragment transfer preserves the existing maximum Fact
size while keeping process frames small. It does not grant another Store reader
or permit access beyond the captured horizon.

A draft's validated snapshot may reference unpublished CAS data. A proposal can
therefore retain a process-local owner for the complete snapshot. Baseline clones
and first-publication admission preserve it until canonical publication; replacing
a domain replaces its retained owner after validation. Retention does not expose
a Store writer or turn a projection into a state owner.

Commands have no originating Turn terminal fence. Their move-only publication
owner therefore retains a read-only outcome for reconciliation. Prewrite drop
proves refusal; entering Store admission disables that inference. Exact canonical
controls or applied draft state are recorded before notification. An unresolved
outcome permits positive canonical receipt lookup, never rollback from absence.
Command-produced text has explicit invocation provenance and is bound into the
same domain receipt. Draft text stays unpublished until the first mailbox submit.

## Alternatives considered

Native ABI polling would need another Rust queue, wake mechanism and cancellation
owner while preserving the callback gate's serial constraints. A second Node
database would split durable authority. Encoding journal metadata into the
model-visible Tool result would couple the provider contract to host bookkeeping.
Using Tool runtime `commit` as acknowledgement would confuse cleanup with durable
publication. Inferring rejection from an absent receipt before terminal admission
could race an already owned Kernel commit.

## Consequences

The process can overlap requests and reverse callbacks, but it is not a sandbox:
heap and CPU are not confined by the protocol's I/O bounds. A failed watchdog
terminates the generation, preserving effect uncertainty. Payload trees are
expanded under PackageStore's immutable object, lease and GC authority; large npm
closures do not use an independent mutable cache.
The standard package factory resolves a versioned descriptor from an exact
PackageStore digest. Descriptor paths can name the declaring package or its
exact direct dependencies; installation validates them before selection. The
external executable and complete environment remain operator configuration.
The connection's Process-settlement waiter retains the package closure even
after its last caller disappears. This avoids releasing files after merely
sending termination, before the process and pipes have settled.

These mechanisms do not by themselves establish full DSH compatibility. CLI
importer installation
and workbench service integration require their own owning contracts and tests.
The external DSH packages remain in their independent repository.

Fork transformation runs against a captured historical domain/Fact cut and
publishes only a child baseline. It never invokes business callbacks to recreate
state. Last windows need exact versioned projection seeds because refolding from
empty would lose Plan state preceding the selected conversation. Full follows
the source's inherited history: keeping ancestor messages only in plugin views
while omitting them from the native model context would expose conflicting
conversations. Context, maintenance and export therefore share that selection
semantics. Last remains a selection of completed local parent Turns; it does not
expand a window with earlier ancestor messages. This changes derived context
semantics and invalidates older Context caches, not the canonical Session format.

The workbench can populate its initial Session tree without acquiring a Kernel
or activating an Agent. Its request-scoped reader pages the caller-visible
Session catalog and reads the indexed durable activity summaries. The browser
workbench is a separate consumer of the Session owner. Making it depend on
the Agent adapter's in-memory registry would hide cold Sessions and would
start execution resources during navigation. The original Typert service and
codec can carry this read without a second browser transport.

Registry lookup and import have different lifetimes from the application.
Installing npm as an independent immutable object lets the application open
without acquiring a build toolchain. The importer retains the exact runtime
and npm dependencies needed to prepare its result. Explicit build approval
belongs to the original package manager's per-dependency policy. An approved
script runs with the local operator's OS authority in private staging before
PackageStore publishes its output.

A process lease protects files during execution. The profile's selected
packages must also survive a period with no running process, including when
another profile imports a different version under the same package name.
PackageStore owns exact per-profile GC roots under its existing writer lock.
Retaining old and candidate roots before replacing the portable view allows
restart to reconcile the Host journal after either publication boundary.
Importers invoked by an application reserve their candidate closures without
selecting packages for the CLI. The running application and its configured
importer retain their own bootstrap roots, independently of market changes.
