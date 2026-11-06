---
name: Application tool registrations belong to their producing model request
---

## Problem

The preset owns a long-lived immutable catalog. A market application has a
different lifetime. An enabled plugin may disappear or replace its registration
while a model request or its approval is pending.

## Decision

The earlier
[preset decision](../../implemented/architecture/2026-08-30-agent-preset-generations.md)
identified the risk of looking up a name again between declaration and start.
A separately sealed supplemental catalog binds both operations to the same
registration. The original application registry still decides whether that
registration remains live. The producing request records its selected schema,
external registration and native result owner.

## Alternatives considered

Publishing every marketplace update as a new preset generation leaves active
Sessions on the old preset. Reopening a tool by name after approval can
authorize a replacement body with the old tool's arguments and approval.

## Consequences

Model retries share this view. A later series observes
new registrations. A cold application can take longer to verify than the
BeforeStep deadline, so a request uses its preset tools and records the unavailable
source. It can try the application again on its next preparation.

The Kernel repairs open Turns before a browser or application Worker becomes
available. Querying a restarted process at that point would make recovery depend
on the same Session services being initialized. The product therefore supplies
a read-only receipt directory for each named source. A matching record lets
the Kernel recover the original output without dispatching plugin code.
Bounded retention and a process exit before recording its result can both
leave the effect uncertain. Startup still closes the interrupted Turn.

The native Tool intent already owns durable admission. The application does not
write another pending marker before entering a plugin. The native transport sends
start once to its captured process; a lost reply and a replacement process permit
only outcome queries. Start is bound to a live view and its original registration,
so an old invocation cannot enter a replacement plugin. Bounded live records retain
unknown results and duplicate-call identities until retirement or eviction. Only
settled results need an application file, preserving cold recovery before the
Worker starts while removing one atomic write from every execution.

## Tool hook input and recovery

A tool can finish while its siblings are still running. Entering its messages
as model input at that instant would violate the Step's active-effect rule.
The result therefore retains the messages. The claim's context cursor derives
the pending list from results and entered inputs. The next preparation publishes
the entered messages before other plugin additions, after the batch settles.
Parent and fork each consume the list represented by their own history.

Keeping a second queue in the extension journal worked for the Agent adapter
but left the market application with a separate recovery obligation. The
Kernel can recover a completed market result before its process starts, and
existing Session baselines already pin their domain catalogs. Using the
result as the durable source gives both execution paths the same recovery
behavior without changing preset domain membership.

A single preparation can receive more messages than the Store admits Facts in
an append. Multiple messages therefore share one entered Fact while each keeps
its source, ordinal and text blocks. Model requests and application history
expand it back into separate user-role messages. The [Session contract](../../../../crates/rsi-agent/session-protocol/README.md)
owns the format.
