---
name: Run application middleware within the native Tool settlement
---

## Problem

Claude Code Mods correlates the original around and post-execute hooks using
one DSH execution token. A hook can deny a shell call or rewrite its output
after the shell exits. Native Tool recovery reads the registration owner's
retained result.

## Decision

The [Tool owner](../../../../crates/rsi-tools/core/README.md) runs the captured
application around an already-admitted body, within its timeout and retention
scope. The [application listener](../../../../apps/serve/README.md) carries the
continuation on its own connection. A suspended shell leaves browser and
application-owned Tool slots available. Native process approval and enforcement
stay with the shell.

## Alternatives considered

Editing the model's ToolResult after native settlement would expose the
original output on recovery. Registering the shell a second time in the
application would change its execution owner and approval path.

Middleware can finish with advice after refusing an unstarted call. The
rejection owns those messages because it is the durable outcome the next
request or a fork will replay. Recording a separate body result would claim an
execution that never took place. The user's refusal also survives a lost
middleware response.

## Consequences

Only the native continuation can start the shell. Disconnect during execution
cancels and drains it. A retained or committed outcome includes the application's
final presentation. When the application changes presentation alone, the native
owner keeps the exact canonical value. Losing the whole Host before durable
settlement can leave the outcome unknown. Replaying the shell could repeat its
side effects.
