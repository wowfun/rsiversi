---
name: Application Session readers mirror native history without a second writer
---

## Problem

DSH workbench plugins use synchronous Session readers and asynchronous
persistence handles even when no Agent is executing. Header-only adapters make
ordinary phase queries fail and prevent cold Side Chat transcripts from loading.
A second JavaScript execution loop would introduce a competing owner beside
the native Kernel. Restricting every plugin lookup and mutation to an HTTP
request also prevents ordinary application-owned Agent work.

## Decision

The [application event-owner decision](2026-11-03-application-session-event-owner.md)
partially supersedes reconstruction of all DSH positions from native order.
Shared readers and attached Agents now retain one application log, which accepts
model-selection events synchronously and correlates their native counterparts.
The native execution and request-authority boundaries below still apply.

Serve supplies an authorized native history cut, including its preset and
Fact/control revision. The external application projects that cut into immutable
DSH Session values. Request-local synchronous lookups and read-only persistence
handles use the same projection. Each new read checks the original native request;
cached content is not an authorization grant. Explicitly opened workbench
transcripts can retain a native-backed Agent in the original Cordis registry.
Persisted opens obtain their immutable model baseline and Fact/control cut from
authorized stored metadata. Like DSH's JSONL read handles, they do not need a
live Agent projection to reconstruct history. Requesting a full model projection
first would admit runtime composition work whose current selection is unused:
the history projector already derives it from canonical controls. Reusing the
captured cut also avoids a second opening call on refresh. Unpublished drafts
still need their live selection because they have no durable control history.
Catalog listings omit the baseline payload; no second stored projection is added.
Inbox metadata contains at most 64 typed routing entries and fits one process
reply. Capture returns it directly and retains a reader only when acceptance
reads need that immutable cut. This avoids
three serialized process calls and a resource slot for the common empty inbox.
An incomplete message retains only its current encoded body, avoiding repeated
Store reads and serialization without retaining every queued body. Request
retirement still invalidates access to all retained readers.
That application-owned identity supports plugin lookups after a browser closes;
native callbacks use the selected application's owner capability.
Native history observation emits committed Session events through the original
plugin event bus. A shared plugin notification socket needs no Session in its
connection URL: the selected trusted application owns its subscription frames,
and transcript reads still cross the native Session boundary.

An attached Agent also forwards the native model stream through DSH's ordinary
transient assistant notifications. The original Side Chat buffers those frames;
committed Session events alone cannot show an unfinished answer. The workbench
and Agent share the native stream converter. Attachment and read reconnection
replay the available prefix, while final publication supplies any remaining chunks
before the committed end notification. Stream observations do not create another
model request or synthetic durable events. Cancellation releases reader resources
under the same native callback without inheriting the cancelled read signal; it
does not grant another caller access or cancel execution owned by that Session.
The resident observation retains authority without retaining one open publication.
Notification-triggered writes have finite owner activities and keep per-Agent
ordering. Otherwise a per-publication write bound becomes a lifetime quota, and
one failed write can suppress unrelated notifications indefinitely. Stopping a
read retires its observation; Agent disposal separately drains accepted writes.

The registry factory supplies native history-cut manual forks and cold resume.
The native manual fork seam exposes the existing `ForkPoint` rather than only
a completed Turn ID. A before point selects the stable prefix without waiting
for an in-flight source Turn, using the same Store boundary and domain transform
as existing Agent forks. History provenance remains separate from execution
ownership: a manual fork is still an independent root, with no completion
notification that could wake its source.
The caller's Cordis scope owns setup, publication and disposal, while Kernel
alone drives execution. Agent history and pending input share one native cut.
Scoped tools retain the exact Agent produced by setup. Teardown waits for native
writes and the owned active Turn before releasing scoped registrations. Merely
attaching a parent mirror does not acquire ownership of its Headless execution.

The native create receipt also carries the accepted draft's list summary. The
DSH adapter announces it through the original Session-added event, so connected
owner browsers can display the draft without a second catalog query or Agent.
The application contract is owned by [Serve](../../../../apps/serve/README.md).
An application request may stage synchronous caller actions for serial native
submission. Its HTTP response waits for those actions to settle. This adapts the
caller's synchronous scheduling contract without inventing a synchronous durable
receipt or moving the native queue into JavaScript. The response barrier cannot
roll back actions already committed before a later failure.
The native draft catalog carries its retained completed-turn count. Creation
events and later list queries derive blankness from that same immutable history
selection, so a browser reconnect does not hide an unpublished fork. The count
does not require opening the parent transcript or publishing the child.

The application inbox retains one native inspection and reads only the exact
acceptances named by it. Durable pending work remains visible after a Worker
restart. Queue withdrawal uses its native compare-and-set receipt, because
message cancellation can cancel a Turn when execution wins the claim race.
Snapshot reads do not freeze execution or turn a later rejection into success.
Draft reads include the exact held Command inputs under the same revision lock.
Their assigned identities survive first publication, but they have no durable
acceptance sequence before it. Treating all drafts as empty would lose already
admitted context and could duplicate a Side Chat boundary on a second request.

DSH's inbox mutates its current log synchronously and lets persistence buffer
asynchronously. The application mirrors this local behavior with pending
withdrawal intents, while retaining native queue ownership. Synchronous remove
returns whether the displayed input was selected; clear selects the displayed
batch. The finite response waits for native success. A claim race fails that
response and refreshes the mirror, rather than cancelling execution or reporting
a committed deletion. The overlay owns no execution and disappears on settlement.
Native draft withdrawal removes exact held identities under publication admission;
command retries retain their original receipts and cannot resurrect those inputs.
Ordinary durable withdrawal accepts every source. Editing still requires Human
provenance. The initial input of a ProgramRun-owned child requires its existing
graph settlement through Program cancellation. That relation comes from its
Header and admission, not the similarly named source of a completion notice.

Mailbox events are projected from the native control log, at its stored Fact
anchors. The application does not turn a changed current snapshot into invented
historical events. Large acceptance bodies are read by their exact immutable
control identity through fragments. Original DSH inbox splices then support both
replay and event-only plugin observers, in the native inspection's acceptance order.
Only the child's own controls participate: manual fork history is conversational
context, not inherited pending work.

## Alternatives considered

A parallel execution writer would create competing execution histories. Starting an
execution loop for every read would turn navigation into execution. Keeping
all Agent access request-local makes plugin timers and Side Chat unavailable
without improving the isolation of trusted local plugins. Polling the full Session
catalog after creation adds failure and work after the command already succeeded.
Immutable native receipts and bounded read mirrors satisfy these observational
contracts without those owners.

## Consequences

Unpublished drafts can be viewed synchronously but are absent from the durable
persistence contract. Read handles cannot append or flush. A fork projection adds
the original DSH inherited-history delimiter from its immutable child Header;
this is presentation of native lineage rather than a new native Fact. These
readers remain read-only; the separate native-backed Agent factory supplies
creation and submission. Arbitrary synthetic seed events and synchronous appends
other than the integrated model-selection event still need their owning contracts.
They remain implementation gaps, not safety policy or reasons to claim complete
DSH Agent compatibility.
